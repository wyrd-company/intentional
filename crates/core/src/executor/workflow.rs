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
use crate::executor::recipe::{
    resolve_publications, Packager, SelectedPublication, PRIMARY_TARGET,
};
use crate::model::PublisherKind;
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
const APP_TOKEN_ACTION: &str =
    "actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349";

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
        jobs.push((id, publisher_job(namespaces, &verify, publication, config)));
    }

    // Tag verification seeds the graph unconditionally so that closure can
    // never mint repository authority concurrently with the check that proves
    // the tag it is closing. Gate contributors are dependencies of assembly so
    // their contributions are available to it, and of closure so the configured
    // gate governs the final authority transition.
    let mut assemble_needs = vec![verify.clone()];
    assemble_needs.extend(publisher_jobs.iter().cloned());
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
fn scalar(value: &str) -> String {
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
    let mut rendered = template
        .replace("@JOB@", &namespaces.job)
        .replace("@ENVVAR@", &namespaces.envvar)
        .replace("@ENVIRONMENT@", &namespaces.environment)
        .replace("@SENTINEL@", OWNERSHIP_SENTINEL)
        .replace("@CONTRACT@", WORKFLOW_CONTRACT)
        .replace("@CHECKOUT@", CHECKOUT_ACTION)
        .replace("@UPLOAD@", UPLOAD_ARTIFACT_ACTION)
        .replace("@DOWNLOAD@", DOWNLOAD_ARTIFACT_ACTION)
        .replace("@APP_TOKEN@", APP_TOKEN_ACTION);
    for (placeholder, value) in extra {
        rendered = rendered.replace(placeholder, value);
    }
    serde_yaml::from_str(&rendered).map_err(|error| {
        WorkflowDiagnostic::new(
            "job-template-invalid",
            format!("a managed job template did not render to valid YAML: {error}"),
        )
    })
}

/// Publisher job derived from one resolved publication and its recipe.
fn publisher_job(
    namespaces: &PrefixNamespaces,
    verify: &str,
    publication: &SelectedPublication,
    config: &Config,
) -> std::result::Result<Value, WorkflowDiagnostic> {
    let unit = &config.release_units[&publication.release_unit];
    let target = if publication.target == PRIMARY_TARGET {
        String::new()
    } else {
        format!(" --target {}", publication.target)
    };
    let identity = publication.identity();
    let slug = identifier(&identity);
    // Publisher credentials stay in the repository-owned recipe steps, so the
    // destination readback they perform reaches the portable command as a
    // schema-backed observation rather than as a second verification path.
    let verify_command = format!(
        "intentional verify publication --release-unit {} --publisher {}{target} --observation \"${{{{ runner.temp }}}}/{}observation/{slug}.yml\" --output \"${{{{ runner.temp }}}}/{}evidence/{slug}.yml\"",
        publication.release_unit,
        publication.publisher.as_str(),
        namespaces.job,
        namespaces.job
    );
    job(
        PUBLISH_PUBLISHER_JOB,
        namespaces,
        &[
            ("@NEEDS@", &render_list(&[verify.to_owned()])),
            ("@SLUG@", &slug),
            ("@PUBLISH_NAME@", &scalar(&format!("Publish {identity}"))),
            (
                "@VERIFY_NAME@",
                &scalar(&format!("Verify the {identity} publication")),
            ),
            (
                "@FRAGMENT_NAME@",
                &scalar(&format!("Upload the {identity} evidence fragment")),
            ),
            ("@VERIFY_COMMAND@", &scalar(&verify_command)),
            (
                "@WORKING_DIRECTORY@",
                &scalar(&unit.path.display().to_string()),
            ),
            ("@PACKAGE_COMMAND@", package_command(publication)),
            ("@PERMISSIONS@", &publisher_permissions(publication)),
        ],
    )
}

/// Native command the selected recipe drives for one publication.
const fn package_command(publication: &SelectedPublication) -> &'static str {
    match publication.packager {
        Packager::Npm => "npm publish --provenance --access public",
        Packager::Cargo => "cargo publish --locked",
        Packager::GoReleaser => "goreleaser release --clean",
        Packager::Buildx => "docker buildx build --push --provenance true --sbom true .",
        Packager::DevContainerCli => {
            "devcontainer features publish --namespace \"${GITHUB_REPOSITORY}\" ."
        }
    }
}

/// Least privilege one publication's destination requires.
fn publisher_permissions(publication: &SelectedPublication) -> String {
    let mut scopes = vec!["  contents: read\n".to_owned()];
    let packages = matches!(
        (publication.publisher, publication.target.as_str()),
        (PublisherKind::Npm, "github") | (PublisherKind::Oci, "ghcr")
    );
    if packages {
        scopes.push("  packages: write\n".to_owned());
    }
    scopes.push("  id-token: write\n".to_owned());
    scopes.concat()
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
    run: intentional release prepare --output "${{ runner.temp }}/@JOB@candidate"
  - name: Upload the release candidate
    uses: @UPLOAD@
    with:
      name: @JOB@candidate
      path: ${{ runner.temp }}/@JOB@candidate
      retention-days: 1
"#;

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
    run: intentional verify handoff "${{ runner.temp }}/@JOB@candidate"
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
    run: intentional verify release-tag
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
  - name: @PUBLISH_NAME@
    working-directory: @WORKING_DIRECTORY@
    run: @PACKAGE_COMMAND@
  - name: @VERIFY_NAME@
    run: @VERIFY_COMMAND@
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
  - name: Assemble the release evidence
    run: >-
      intentional evidence assemble
      --input "${{ runner.temp }}/@JOB@fragments"
      --output "${{ runner.temp }}/@JOB@release"
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
    use crate::executor::fixture::Workspace;

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

    /// The shell of the derived closure job, in the order a runner executes it.
    fn closure_script(root: &Path) -> String {
        let document: Value =
            serde_yaml::from_str(&workflow(root, WorkflowRole::Publish)).expect("result parses");
        document["jobs"]["intentional_close_release"]["steps"]
            .as_sequence()
            .expect("the closure job carries steps")
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
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
}
