// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use intentional_core::{
    assemble, check_executor, check_workspace, compare_workflow, contribute, initialize,
    initialize_executor, prepare_release, verify_handoff, ApplyResult, AssembleRequest, Bump,
    ComparisonStatus, Config, ContributionRequest, ExecutorInitState, InitState, IntentDraft,
    ReleasePlan, StampResult, TagPhase, TagResult, WorkflowIdentity, WorkflowRole, WorkspaceStatus,
    CONFIG_PATH, LOCAL_JOB,
    MISSING_BASELINE_CODE, MISSING_BASELINE_NEXT_ACTION,
};
use semver::Version;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;

const SKILL_DOCUMENT: &str = include_str!("../skills/intentional/SKILL.md");

#[derive(Debug, Parser)]
#[command(
    name = "intentional",
    version,
    about = "Intent-driven polyglot releases"
)]
struct Cli {
    /// Workspace directory.
    #[arg(short = 'C', long, default_value = ".", global = true)]
    directory: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan supported manifests and create the release-unit inventory.
    Init(InitArgs),
    /// Author a pending change intent.
    Add(AddArgs),
    /// Show pending intents and computed release-unit versions.
    Status,
    /// Emit canonical digest-bound release-plan JSON.
    Plan(ChannelArgs),
    /// Materialize versions, changelogs, dependencies, and intent consumption.
    Apply(ChannelDryRunArgs),
    /// Write computed versions into injected projections only.
    Stamp(StampArgs),
    /// Create annotated release records or establish baseline tags.
    Tag(TagArgs),
    /// Validate config, intents, and deterministic planning for CI.
    Check,
    /// Configure and reconcile the repository release protocol.
    #[command(subcommand)]
    Executor(ExecutorCommand),
    /// Construct release protocol artifacts.
    #[command(subcommand)]
    Release(ReleaseCommand),
    /// Construct schema-backed release evidence artifacts.
    #[command(subcommand)]
    Evidence(EvidenceCommand),
    /// Verify release protocol identities, handoffs, and evidence.
    #[command(subcommand)]
    Verify(VerifyCommand),
    /// Print the agent-facing Intentional workflow skill.
    Skill,
}

#[derive(Debug, Subcommand)]
enum ExecutorCommand {
    /// Configure the GitHub executor and explicit publication intent.
    Init(DryRun),
    /// Compare a managed workflow with its derived executor contract.
    Diff(DiffArgs),
    /// Check configured workflows against their derived executor contracts.
    Check,
}

#[derive(Debug, Subcommand)]
enum ReleaseCommand {
    /// Resolve the source and construct its deterministic release candidate.
    Prepare(PrepareArgs),
}

#[derive(Debug, Subcommand)]
enum EvidenceCommand {
    /// Construct one repository-owned evidence contribution bundle.
    Contribute(ContributeArgs),
    /// Assemble publisher fragments and contributions into final release evidence.
    Assemble(AssembleArgs),
}

#[derive(Debug, Subcommand)]
enum VerifyCommand {
    /// Independently verify a prepared release-candidate handoff.
    Handoff(HandoffArgs),
}

#[derive(Debug, Args)]
struct PrepareArgs {
    /// Directory in which to write the release-candidate handoff.
    #[arg(long, value_name = "DIRECTORY")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct ContributeArgs {
    /// Unique namespace assigned to the contribution value.
    #[arg(long)]
    namespace: String,

    /// File containing any valid YAML value for the namespace.
    #[arg(long, value_name = "PATH")]
    value_file: Option<PathBuf>,

    /// Exact file path to include as a GitHub Release attachment.
    #[arg(long = "attachment", value_name = "PATH")]
    attachments: Vec<PathBuf>,

    /// Directory in which to write the contribution bundle.
    #[arg(long, value_name = "PATH")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct AssembleArgs {
    /// Directory containing downloaded publisher, phase-tag, and contribution artifacts.
    #[arg(long, value_name = "PATH")]
    input: PathBuf,

    /// Directory in which to write the closed final-evidence bundle.
    #[arg(long, value_name = "PATH")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct HandoffArgs {
    /// Release-candidate handoff directory to verify.
    #[arg(value_name = "HANDOFF")]
    handoff: PathBuf,
}

#[derive(Debug, Args)]
struct DiffArgs {
    /// Executor role of the workflow being compared.
    #[arg(value_name = "WORKFLOW-ROLE")]
    role: String,

    /// Workflow file to compare instead of the path configured for the role.
    #[arg(long, value_name = "PATH")]
    workflow: Option<PathBuf>,

    /// Representation of the semantic comparison result.
    #[arg(long, default_value = "patch", value_parser = ["patch", "json"])]
    format: String,

    /// Apply the syntax-aware transformation and print the applied comparison.
    #[arg(long)]
    apply: bool,
}

#[derive(Debug, Args)]
struct DryRun {
    /// Print mutations without changing the workspace.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Perform the explicit authority handoff from a ready Changesets plan.
    #[arg(long)]
    take_over: bool,

    /// Emit stable structured JSON.
    #[arg(long)]
    json: bool,

    /// Print mutations without changing the workspace.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Release-unit bump as `id:major|minor|patch`; repeat for multiple release units.
    #[arg(long = "release-unit")]
    release_units: Vec<String>,

    /// Changelog prose.
    #[arg(long)]
    message: Option<String>,

    /// Print mutations without changing the workspace.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct ChannelArgs {
    /// Release channel, such as beta.
    #[arg(long)]
    channel: Option<String>,
}

#[derive(Debug, Args)]
struct ChannelDryRunArgs {
    /// Release channel, such as beta.
    #[arg(long)]
    channel: Option<String>,

    /// Print mutations without changing the workspace.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct TagArgs {
    /// Release channel, such as beta.
    #[arg(long)]
    channel: Option<String>,

    /// Establish initial annotated tag authority from projections.
    #[arg(long)]
    baseline: bool,

    /// Explicit baseline as `release-unit=X.Y.Z` or `workspace/tag=X.Y.Z`; repeat as needed.
    #[arg(long = "version")]
    versions: Vec<String>,

    /// Executor phase declaration required by configured release tags.
    #[arg(long)]
    phase: Option<String>,

    /// Digest-sealed release plan to verify before creating release tags.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Print mutations without changing the workspace.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct StampArgs {
    /// Prerelease identifier composed with first-parent tag height.
    #[arg(long)]
    prerelease: Option<String>,

    /// Print mutations without changing the workspace.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => return init(&cli.directory, args),
        Command::Add(args) => add(&cli.directory, args),
        Command::Status => status(&cli.directory),
        Command::Plan(args) => plan(&cli.directory, args.channel.as_deref()),
        Command::Apply(args) => apply(&cli.directory, args.channel.as_deref(), args.dry_run),
        Command::Stamp(args) => stamp(&cli.directory, args.prerelease.as_deref(), args.dry_run),
        Command::Tag(args) => tag(&cli.directory, args),
        Command::Check => check(&cli.directory),
        Command::Executor(ExecutorCommand::Init(args)) => {
            return executor_init(&cli.directory, args.dry_run)
        }
        Command::Executor(ExecutorCommand::Diff(args)) => {
            return executor_diff(&cli.directory, args)
        }
        Command::Executor(ExecutorCommand::Check) => return executor_check(&cli.directory),
        Command::Release(ReleaseCommand::Prepare(args)) => prepare(&cli.directory, args),
        Command::Evidence(EvidenceCommand::Contribute(args)) => {
            evidence_contribute(&cli.directory, args)
        }
        Command::Evidence(EvidenceCommand::Assemble(args)) => {
            evidence_assemble(&cli.directory, args)
        }
        Command::Verify(VerifyCommand::Handoff(args)) => handoff(&cli.directory, args),
        Command::Skill => skill(),
    }?;
    Ok(0)
}

fn skill() -> Result<()> {
    print!("{SKILL_DOCUMENT}");
    Ok(())
}

fn prepare(root: &std::path::Path, args: PrepareArgs) -> Result<()> {
    let prepared = prepare_release(root, &args.output)?;
    for projection in prepared.projections() {
        println!("{projection}");
    }
    Ok(())
}

fn handoff(root: &std::path::Path, args: HandoffArgs) -> Result<()> {
    let verified = verify_handoff(root, &args.handoff)?;
    println!("release handoff verified");
    for projection in verified.projections() {
        println!("{projection}");
    }
    Ok(())
}

fn executor_init(root: &std::path::Path, dry_run: bool) -> Result<u8> {
    let result = initialize_executor(root)?;
    println!("executor initialization state: {}", result.state);
    for operation in &result.operations {
        println!("{operation}");
    }
    if dry_run {
        // A dry run writes nothing, so name every file it would have written
        // and print the plan rather than pointing at a file that is absent.
        for relative in result.planned_writes() {
            println!("would write {}", relative.display());
        }
        println!("--- {}", result.path.display());
        print!("{}", result.plan.to_yaml()?);
    } else {
        println!("plan: {}", result.path.display());
    }
    result.apply(root, dry_run)?;
    Ok(if result.state == ExecutorInitState::NeedsInput {
        2
    } else {
        0
    })
}

fn executor_diff(root: &std::path::Path, args: DiffArgs) -> Result<u8> {
    let role = args
        .role
        .parse::<WorkflowRole>()
        .map_err(anyhow::Error::msg)?;
    let comparison = compare_workflow(root, role, args.workflow.as_deref())?;
    let comparison = if args.apply && comparison.changed() {
        comparison.apply(root)?
    } else {
        comparison
    };
    if args.format == "json" {
        println!("{}", comparison.to_json()?);
    } else {
        print_patch(&comparison);
    }
    if comparison.status == ComparisonStatus::Blocked {
        bail!("the {role} workflow comparison is blocked; resolve the reported diagnostics",);
    }
    Ok(0)
}

fn print_patch(comparison: &intentional_core::WorkflowComparison) {
    println!(
        "{} workflow {} ({})",
        comparison.role,
        comparison.path.display(),
        comparison.status
    );
    println!("input digest: {}", comparison.input_digest);
    for diagnostic in &comparison.diagnostics {
        match &diagnostic.path {
            Some(path) => println!("[{}] {} at {path}", diagnostic.code, diagnostic.message),
            None => println!("[{}] {}", diagnostic.code, diagnostic.message),
        }
    }
    if comparison.applied {
        println!("applied the transformation");
    }
    print!("{}", comparison.patch);
}

fn executor_check(root: &std::path::Path) -> Result<u8> {
    let result = check_executor(root)?;
    for publication in &result.publications {
        println!("publication: {publication}");
    }
    for finding in &result.findings {
        println!("finding: {finding}");
    }
    if result.conforms() {
        println!("executor check passed");
        return Ok(0);
    }
    Ok(1)
}

fn evidence_contribute(root: &std::path::Path, args: ContributeArgs) -> Result<()> {
    let value_file = args.value_file.map(|path| resolve(root, path));
    let attachments = args
        .attachments
        .into_iter()
        .map(|path| resolve(root, path))
        .collect::<Vec<_>>();
    let output = resolve(root, args.output);
    let bundle = contribute(&ContributionRequest {
        namespace: &args.namespace,
        value_file: value_file.as_deref(),
        attachments: &attachments,
        output: &output,
        job: &optional_environment("GITHUB_JOB", LOCAL_JOB)?,
        run_attempt: optional_numeric_environment("GITHUB_RUN_ATTEMPT", 1)?,
    })?;
    println!("bundle: {}", bundle.path.display());
    println!("manifest: {}", bundle.manifest_path.display());
    for attachment in &bundle.manifest.attachments {
        println!("attachment: {} {}", attachment.name, attachment.sha256);
    }
    // The contribution Action uploads the bundle under this name; it is the
    // only value the Action needs and it is never exposed as an Action output.
    println!("artifact-name: {}", bundle.artifact_name);
    Ok(())
}

fn evidence_assemble(root: &std::path::Path, args: AssembleArgs) -> Result<()> {
    let input = resolve(root, args.input);
    let output = resolve(root, args.output);
    let assembly = assemble(&AssembleRequest {
        root,
        input: &input,
        output: &output,
        workflow: WorkflowIdentity {
            repository: environment("GITHUB_REPOSITORY")?,
            workflow: environment("GITHUB_WORKFLOW")?,
            run_id: numeric_environment("GITHUB_RUN_ID")?,
            run_attempt: numeric_environment("GITHUB_RUN_ATTEMPT")?,
            commit: environment("GITHUB_SHA")?,
        },
    })?;
    println!("evidence: {}", assembly.evidence_path.display());
    for attachment in &assembly.attachments {
        println!("attachment: {attachment}");
    }
    Ok(())
}

fn resolve(root: &std::path::Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn environment(name: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => {
            bail!("{name} identifies the assembling workflow run and must be set")
        }
        Err(error) => bail!("{name} is set to a value this platform cannot read: {error}"),
    }
}

/// Read a workflow variable that is absent only outside GitHub Actions.
///
/// A value that is present but unreadable is an error rather than a fallback,
/// because silently substituting the local default would misattribute the
/// contribution it identifies.
fn optional_environment(name: &str, fallback: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Ok(fallback.to_owned()),
        Err(error) => bail!("{name} is set to a value this platform cannot read: {error}"),
    }
}

/// Read a required numeric workflow variable, rejecting a present but unusable value.
fn numeric_environment<T: std::str::FromStr>(name: &str) -> Result<T> {
    let value = environment(name)?;
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("{name} must be a whole number; got {value:?}"))
}

/// Read an optional numeric workflow variable, rejecting a present but unusable value.
///
/// A malformed run attempt would otherwise claim attempt one, so a re-run would
/// either collide with the first attempt's artifact name or fail to supersede it.
fn optional_numeric_environment<T: std::str::FromStr>(name: &str, fallback: T) -> Result<T> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(fallback),
        Err(error) => bail!("{name} is set to a value this platform cannot read: {error}"),
        Ok(value) => value
            .parse()
            .map_err(|_| anyhow::anyhow!("{name} must be a whole number; got {value:?}")),
    }
}

fn check(root: &std::path::Path) -> Result<()> {
    check_workspace(root)?;
    println!("check passed");
    Ok(())
}

fn tag(root: &std::path::Path, args: TagArgs) -> Result<()> {
    let phase = args.phase.as_deref().map(parse_phase).transpose()?;
    let mut explicit = BTreeMap::new();
    for value in args.versions {
        let (id, version) = value
            .split_once('=')
            .with_context(|| format!("baseline version must be id=X.Y.Z; got {value}"))?;
        let version = Version::parse(version)?;
        if explicit.insert(id.to_owned(), version).is_some() {
            bail!("baseline version {id} was specified more than once");
        }
    }
    if args.baseline && args.channel.is_some() {
        bail!("--baseline and --channel cannot be combined");
    }
    if args.baseline && args.plan.is_some() {
        bail!("--baseline and --plan cannot be combined");
    }
    if !args.baseline && !explicit.is_empty() {
        bail!("--version is valid only with --baseline");
    }
    let result = if args.baseline {
        TagResult::build_baseline(root, &explicit)?
    } else {
        let plan_path = args.plan.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        });
        TagResult::build_with_plan(root, args.channel.as_deref(), phase, plan_path.as_deref())?
    };
    for operation in result.operations() {
        println!("{operation}");
    }
    result.apply(root, args.dry_run)?;
    Ok(())
}

fn stamp(root: &std::path::Path, prerelease: Option<&str>, dry_run: bool) -> Result<()> {
    let result = StampResult::build(root, prerelease)?;
    for operation in result.operations() {
        println!("{operation}");
    }
    result.apply(root, dry_run)?;
    Ok(())
}

fn apply(root: &std::path::Path, channel: Option<&str>, dry_run: bool) -> Result<()> {
    let result = ApplyResult::build(root, channel)?;
    for operation in result.operations() {
        println!("{operation}");
    }
    result.apply(root, dry_run)?;
    Ok(())
}

fn plan(root: &std::path::Path, channel: Option<&str>) -> Result<()> {
    let plan = ReleasePlan::build(root, channel)?;
    println!("{}", plan.to_canonical_json()?);
    Ok(())
}

fn init(root: &std::path::Path, args: InitArgs) -> Result<u8> {
    let result = initialize(root, args.take_over)?;
    if args.json {
        println!("{}", result.to_json()?);
    } else {
        println!("initialization state: {}", result.state);
        for operation in &result.operations {
            println!("{operation}");
        }
        if result.plan.is_some() && !args.take_over {
            println!("plan: {}", result.path.display());
        }
    }
    result.apply(root, args.dry_run)?;
    Ok(if result.state == InitState::NeedsInput {
        2
    } else {
        0
    })
}

fn add(root: &std::path::Path, args: AddArgs) -> Result<()> {
    let config = Config::load(root)
        .with_context(|| format!("load {CONFIG_PATH} before adding an intent"))?;
    let release_unit_values = if args.release_units.is_empty() {
        let ids = config.release_units.keys().cloned().collect::<Vec<_>>();
        println!("Release units: {}", ids.join(", "));
        vec![prompt("Release unit (id): ")? + ":" + &prompt("Bump (major|minor|patch): ")?]
    } else {
        args.release_units
    };
    let mut release_units = BTreeMap::new();
    for value in release_unit_values {
        let (id, bump) = value
            .split_once(':')
            .with_context(|| format!("release-unit bump must be id:bump; got {value}"))?;
        let bump = bump.parse::<Bump>().map_err(anyhow::Error::msg)?;
        if release_units.insert(id.to_owned(), bump).is_some() {
            bail!("release unit {id} was specified more than once");
        }
    }
    let message = match args.message {
        Some(message) => message,
        None => prompt("Changelog message: ")?,
    };
    let write = IntentDraft {
        release_units,
        message,
    }
    .plan(root, &config)?;
    println!("write {}", write.path.display());
    write.apply(root, args.dry_run)?;
    Ok(())
}

fn status(root: &std::path::Path) -> Result<()> {
    let status = WorkspaceStatus::load(root)?;
    if status.intents.is_empty() {
        println!("Pending intents: none");
    } else {
        println!("Pending intents: {}", status.intents.join(", "));
    }
    if !status.missing_baselines.is_empty() {
        println!(
            "[{MISSING_BASELINE_CODE}] Missing baseline tags: {}; run {MISSING_BASELINE_NEXT_ACTION}",
            status.missing_baselines.join(", "),
        );
    }
    for issue in &status.tag_record_issues {
        println!("Tag record: {issue}");
    }
    for release_unit in status.release_units {
        println!(
            "{}: {} -> {} ({})",
            release_unit.id, release_unit.current, release_unit.next, release_unit.bump
        );
    }
    if status.drift.is_empty() {
        println!("Drift: none");
    } else {
        println!("Drift:");
        for drift in status.drift {
            println!(
                "  {} {}: manifest {} != tag {}",
                drift.release_unit,
                drift.file.display(),
                drift.actual,
                drift.expected
            );
        }
    }
    Ok(())
}

fn parse_phase(value: &str) -> Result<TagPhase> {
    match value {
        "before-publication" => Ok(TagPhase::BeforePublication),
        "after-publication" => Ok(TagPhase::AfterPublication),
        _ => bail!("phase must be before-publication or after-publication; got {value}"),
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim().to_owned())
}
