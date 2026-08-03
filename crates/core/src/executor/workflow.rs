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
    /// Reasons a blocked comparison produced no transformation.
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

    let output = match reconcile(document, &contract) {
        Ok(output) => output,
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
        diagnostics: Vec::new(),
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
fn reconcile(
    mut document: Document,
    contract: &WorkflowContract,
) -> std::result::Result<String, WorkflowDiagnostic> {
    // `on: push` and `on: [push, tag]` are shorthand for a trigger mapping.
    // Expanding them first means adding a required trigger never discards the
    // repository's own.
    if let Some(current) = document.get(&["on"]).map_err(unparsable)? {
        if let Some(expanded) = expanded_triggers(&current) {
            document.set(&["on"], &expanded).map_err(unparsable)?;
        }
    }
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
    if !read_only_permissions(document.get(&["permissions"]).map_err(unparsable)?.as_ref()) {
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
    // A transformation that does not parse is never offered to the user.
    Document::parse(&output).map_err(|error| {
        WorkflowDiagnostic::new(
            "transformation-invalid",
            format!("the derived transformation is not valid YAML: {error}"),
        )
    })?;
    Ok(output)
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
        WorkflowRole::Release => Ok(release_contract(&namespaces, &gates)),
        WorkflowRole::Publish => publish_contract(root, config, &namespaces, &gates),
    }
}

fn release_contract(namespaces: &PrefixNamespaces, gates: &[String]) -> WorkflowContract {
    let prepare = format!("{}prepare", namespaces.job);
    let release = format!("{}release", namespaces.job);
    let mut needs = vec![prepare.clone()];
    needs.extend(gates.iter().cloned());
    WorkflowContract {
        namespaces: namespaces.clone(),
        triggers: vec![(
            vec!["on".to_owned(), "workflow_dispatch".to_owned()],
            Value::Null,
        )],
        concurrency: concurrency(&namespaces.environment),
        jobs: vec![
            (prepare, job(RELEASE_PREPARE_JOB, namespaces, &[])),
            (
                release,
                job(
                    RELEASE_AUTHORITY_JOB,
                    namespaces,
                    &[("@NEEDS@", &render_list(&needs))],
                ),
            ),
        ],
    }
}

fn publish_contract(
    root: &Path,
    config: &Config,
    namespaces: &PrefixNamespaces,
    gates: &[String],
) -> std::result::Result<WorkflowContract, Vec<WorkflowDiagnostic>> {
    let mut diagnostics = Vec::new();
    let patterns = config
        .workspace_tags
        .values()
        .map(|tag| Value::String(tag.template.replace("{version}", "*")))
        .collect::<Vec<_>>();
    if patterns.is_empty() {
        diagnostics.push(WorkflowDiagnostic::at(
            "release-tag-undefined",
            "the publish workflow is triggered by the global release tag; configure a workspace tag"
                .to_owned(),
            "workspace-tags",
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

    // Gate contributors are dependencies of assembly so their contributions are
    // available to it, and of closure so the configured gate governs the final
    // authority transition.
    let mut assemble_needs = publisher_jobs.clone();
    assemble_needs.extend(gates.iter().cloned());
    if assemble_needs.is_empty() {
        assemble_needs.push(verify.clone());
    }
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
        jobs,
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
fn job(template: &str, namespaces: &PrefixNamespaces, extra: &[(&str, &str)]) -> Value {
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
    serde_yaml::from_str(&rendered).expect("maintained job templates are valid YAML")
}

/// Publisher job derived from one resolved publication and its recipe.
fn publisher_job(
    namespaces: &PrefixNamespaces,
    verify: &str,
    publication: &SelectedPublication,
    config: &Config,
) -> Value {
    let unit = &config.release_units[&publication.release_unit];
    let target = if publication.target == PRIMARY_TARGET {
        String::new()
    } else {
        format!(" --target {}", publication.target)
    };
    job(
        PUBLISH_PUBLISHER_JOB,
        namespaces,
        &[
            ("@NEEDS@", &render_list(&[verify.to_owned()])),
            ("@IDENTITY@", &publication.identity()),
            ("@SLUG@", &identifier(&publication.identity())),
            ("@RELEASE_UNIT@", &publication.release_unit),
            ("@PUBLISHER@", publication.publisher.as_str()),
            ("@TARGET_OPTION@", &target),
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
      persist-credentials: false
  - name: Download the release candidate
    uses: @DOWNLOAD@
    with:
      name: @JOB@candidate
      path: ${{ runner.temp }}/@JOB@candidate
  - id: @JOB@handoff
    name: Verify the release candidate handoff
    run: intentional verify handoff --candidate "${{ runner.temp }}/@JOB@candidate"
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
      persist-credentials: false
  - name: Publish @IDENTITY@
    working-directory: @WORKING_DIRECTORY@
    run: @PACKAGE_COMMAND@
  - name: Verify the @IDENTITY@ publication
    run: >-
      intentional verify publication
      --release-unit @RELEASE_UNIT@
      --publisher @PUBLISHER@@TARGET_OPTION@
      --output "${{ runner.temp }}/@JOB@evidence/@SLUG@.yml"
  - name: Upload the @IDENTITY@ evidence fragment
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
    run: |
      set -euo pipefail
      gh release upload "${@ENVVAR@GLOBAL_TAG}" \
        "${{ runner.temp }}/@JOB@release"/* --clobber
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
      primary: { role: primary, template: '{id}@{version}' }
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
                    "release-units:\n  component.one:\n    path: one\n    cargo: {}\n    tags:\n      primary: { role: primary, template: 'one@{version}' }\n  component_one:\n",
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
