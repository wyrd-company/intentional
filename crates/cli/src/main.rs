// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use intentional_core::{
    assemble, check_executor, check_workspace, compare_workflow, contribute, initialize,
    initialize_executor, prepare_release, record_built_subject, verify_handoff, verify_publication,
    verify_release_observed, verify_release_tag, ApplyResult, AssembleRequest, Bump,
    CheckoutContext, ComparisonStatus, Config, ContributionRequest, DestinationObserver,
    ExecutorInitState, GhReleaseSource, InitState, IntentDraft, ObservedPublications,
    PublisherKind, ReleasePlan, ReleaseSource, StampResult, SystemClock, TagPhase, TagResult,
    VerifyPublicationRequest, WorkflowIdentity, WorkflowRole, WorkspaceStatus, CONFIG_PATH,
    LOCAL_JOB, MISSING_BASELINE_CODE, MISSING_BASELINE_NEXT_ACTION,
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
    /// Record one built publishable subject produced from the released commit.
    BuiltSubject(BuiltSubjectArgs),
    /// Construct one repository-owned evidence contribution bundle.
    Contribute(ContributeArgs),
    /// Assemble publisher fragments and contributions into final release evidence.
    Assemble(AssembleArgs),
}

#[derive(Debug, Subcommand)]
enum VerifyCommand {
    /// Independently verify a prepared release-candidate handoff.
    Handoff(HandoffArgs),
    /// Verify the current global release tag before publication.
    ReleaseTag,
    /// Verify one destination publication and write its affirmative evidence.
    Publication(PublicationArgs),
    /// Verify an Intentional GitHub Release and its evidence.
    Release(ReleaseArgs),
}

#[derive(Debug, Args)]
struct PublicationArgs {
    /// Release-unit identifier whose publication is verified.
    #[arg(long)]
    release_unit: String,

    /// Configured publisher adapter identifier.
    #[arg(long)]
    publisher: PublisherKind,

    /// Publisher target selector; a publisher with a primary target accepts omission.
    #[arg(long)]
    target: Option<String>,

    /// Publication-observation document the recipe's readback steps produced.
    #[arg(long, value_name = "PATH")]
    observation: PathBuf,

    /// File in which to write schema-backed publisher evidence.
    #[arg(long, value_name = "PATH")]
    output: PathBuf,

    /// Draft-Release asset handoff a draft-dependent publisher consumed.
    #[arg(long, value_name = "PATH")]
    draft_handoff: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ReleaseArgs {
    /// Release version whose GitHub Release is verified.
    #[arg(value_name = "VERSION")]
    version: String,

    /// Perform post-closure destination readback in addition to recorded evidence.
    #[arg(long)]
    live: bool,

    /// Directory of post-closure observations a public consumer client produced.
    #[arg(long, value_name = "PATH", requires = "live")]
    observations: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PrepareArgs {
    /// Directory in which to write the release-candidate handoff.
    #[arg(long, value_name = "DIRECTORY")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct BuiltSubjectArgs {
    /// Release-unit identifier the built subject belongs to.
    #[arg(long)]
    release_unit: String,

    /// Subject identity every configured destination resolves.
    #[arg(long)]
    identity: String,

    /// File or directory holding the bytes the build produced.
    #[arg(long, value_name = "PATH")]
    subject: PathBuf,

    /// File in which to write the schema-backed built-subject document.
    #[arg(long, value_name = "PATH")]
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
    /// Prepared release-candidate handoff identifying the release being closed.
    #[arg(long, value_name = "PATH")]
    candidate: PathBuf,

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

    /// Directory of built-subject and publisher-evidence documents the phase seals.
    #[arg(long, value_name = "PATH", requires = "phase")]
    evidence: Option<PathBuf>,

    /// Directory in which to write the phase evidence the declared phase sealed.
    #[arg(long, value_name = "PATH", requires = "phase")]
    sealed_output: Option<PathBuf>,

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
        Command::Evidence(EvidenceCommand::BuiltSubject(args)) => {
            evidence_built_subject(&cli.directory, args)
        }
        Command::Evidence(EvidenceCommand::Contribute(args)) => {
            evidence_contribute(&cli.directory, args)
        }
        Command::Evidence(EvidenceCommand::Assemble(args)) => {
            evidence_assemble(&cli.directory, args)
        }
        Command::Verify(VerifyCommand::Handoff(args)) => handoff(&cli.directory, args),
        Command::Verify(VerifyCommand::ReleaseTag) => release_tag(&cli.directory),
        Command::Verify(VerifyCommand::Publication(args)) => publication(&cli.directory, args),
        Command::Verify(VerifyCommand::Release(args)) => release(&cli.directory, args),
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

fn release_tag(root: &std::path::Path) -> Result<()> {
    let verified = verify_release_tag(root)?;
    println!("global release tag verified");
    for projection in verified.projections() {
        println!("{projection}");
    }
    Ok(())
}

fn publication(root: &std::path::Path, args: PublicationArgs) -> Result<()> {
    let observation = resolve(root, args.observation);
    let output = resolve(root, args.output);
    let handoff = args.draft_handoff.map(|path| resolve(root, path));
    let clock = SystemClock::new();
    let context = CheckoutContext::new();
    // The Release access exists only to prove a supplied handoff. A publication
    // that consumes no draft asset never reaches it, so a publisher whose
    // consumer path is public still verifies without any GitHub access at all.
    let source = handoff.as_ref().map(|_| GhReleaseSource::new(root));
    let verified = verify_publication(&VerifyPublicationRequest {
        root,
        release_unit: &args.release_unit,
        publisher: args.publisher,
        target: args.target.as_deref(),
        observation: &observation,
        output: &output,
        policy: None,
        clock: &clock,
        context: &context,
        draft_handoff: handoff.as_deref(),
        release_source: source.as_ref().map(|source| source as &dyn ReleaseSource),
    })?;
    if verified.reused {
        println!("reused the sealed publisher evidence");
    }
    // The Action projects exactly this line, so the fragment a consumer reads
    // is the one this invocation proved rather than a path it guessed.
    println!("evidence-path: {}", verified.path.display());
    Ok(())
}

fn release(root: &std::path::Path, args: ReleaseArgs) -> Result<()> {
    let source = GhReleaseSource::new(root);
    // Without observations the only available live read is the closed Release
    // asset, which proves the recorded bytes are unchanged and claims nothing
    // about what an unauthenticated consumer resolves. Supplying them is what
    // makes a draft-dependent publisher's deferred public path checkable.
    let observer = args
        .observations
        .map(|directory| ObservedPublications::new(resolve(root, directory)));
    let verification = verify_release_observed(
        root,
        &args.version,
        args.live,
        &source,
        observer
            .as_ref()
            .map(|observer| observer as &dyn DestinationObserver),
    )?;
    for entry in verification.report() {
        println!("{entry}");
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
        comparison.apply()?
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

fn evidence_built_subject(root: &std::path::Path, args: BuiltSubjectArgs) -> Result<()> {
    let subject = resolve(root, args.subject);
    let output = resolve(root, args.output);
    let built = record_built_subject(root, &args.release_unit, &args.identity, &subject, &output)?;
    println!("subject: {}", built.identity());
    println!("version: {}", built.version);
    println!("digest: {}", built.digest);
    println!("built-subject-path: {}", output.display());
    Ok(())
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
    let candidate = resolve(root, args.candidate);
    let input = resolve(root, args.input);
    let output = resolve(root, args.output);
    let assembly = assemble(&AssembleRequest {
        root,
        candidate: &candidate,
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
    if args.baseline && args.evidence.is_some() {
        bail!("--baseline and --evidence cannot be combined");
    }
    if args.baseline && args.sealed_output.is_some() {
        bail!("--baseline and --sealed-output cannot be combined");
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
        let evidence = args.evidence.map(|path| resolve(root, path));
        TagResult::build_with_plan(
            root,
            args.channel.as_deref(),
            phase,
            plan_path.as_deref(),
            evidence.as_deref(),
        )?
    };
    for operation in result.operations() {
        println!("{operation}");
    }
    result.apply(root, args.dry_run)?;
    if let Some(sealed_output) = args.sealed_output {
        let sealed_output = resolve(root, sealed_output);
        match result.write_sealed_phase_evidence(&sealed_output)? {
            Some(path) => println!("sealed-phase-evidence: {}", path.display()),
            None => bail!(
                "--sealed-output requires a phase that seals evidence; this configuration declares no global release tag to bind it to"
            ),
        }
    }
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

/// Bind the commands managed workflow jobs run to the parser that accepts them.
///
/// Workflow derivation splices `intentional` commands into privileged jobs
/// without consulting the argument parser, and workflow linting validates
/// syntax and action references rather than the contents of a `run:` body. A
/// managed job template can therefore name an argument shape this binary
/// rejects while every other check stays green, and the failure surfaces only
/// when a release runner executes it. These tests derive the workflows the
/// executor produces and parse each generated invocation with the real parser,
/// so this binary is the authority on what a template may say.
///
/// Coverage is the `run:` bodies of derived workflows and the `run:` bodies of
/// every composite Action this repository publishes under `actions/`. Both
/// surfaces name portable commands, and a managed job that invokes one through
/// an Action moves the argument contract from the first surface to the second
/// without weakening it, so both are read here.
///
/// Recognition requires the executable to be named literally. A step that names
/// it through an expression — `${{ env.BIN }} release prepare` or `"$BIN"
/// release prepare` — produces no word this recognizer can match and is
/// invisible to the gate. Replacing an existing invocation that way still
/// fails, because the expected invocation counts are exact; the uncovered case
/// is a net-new step added in an indirect form. Every template and every Action
/// spells the executable literally today, and the counts are what keep that
/// true.
#[cfg(test)]
mod generated_invocations {
    use super::*;
    use clap::CommandFactory;
    use intentional_core::executor::fixture::derived_workflows;

    /// Invocations the managed job templates generate for the fixture workspace.
    ///
    /// None. Managed jobs reach every portable command through a published
    /// Action, because a stock runner carries no `intentional` on PATH and only
    /// the Actions project a command's verified identity lines onto step
    /// outputs. The argument contract those jobs depend on is bound by
    /// [`ACTION_INVOCATIONS`] instead, and which Action each managed job
    /// resolves is proved where the templates live.
    ///
    /// Zero is asserted rather than assumed. A template that reintroduces a
    /// bare `run:` invocation fails here, which is the point: the argument
    /// shape would be bound, but the binary would not be installed and the
    /// step would expose no outputs.
    const GENERATED_INVOCATIONS: usize = 0;

    /// Invocations the published composite Actions run.
    ///
    /// One per Action: `prepare-release`, `verify-handoff`,
    /// `verify-release-tag`, `verify-publication`, `assemble-evidence`,
    /// `contribute`, `record-built-subject`, and `seal-phase-tags`. Asserted
    /// exactly for the same reason the generated count
    /// is: an Action that stops invoking the command, and a recognizer that
    /// stops seeing one, must both fail here rather than bind a smaller surface
    /// than this module claims.
    const ACTION_INVOCATIONS: usize = 8;

    /// Directory holding the composite Actions this repository publishes.
    fn actions_directory() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../actions")
    }

    /// Every `intentional` invocation each published Action runs, by Action.
    fn action_invocations() -> Vec<(String, Vec<Vec<String>>)> {
        let mut actions = std::fs::read_dir(actions_directory())
            .expect("the published Actions directory is readable")
            .filter_map(|entry| {
                let path = entry.expect("directory entry").path().join("action.yml");
                path.is_file().then_some(path)
            })
            .collect::<Vec<_>>();
        actions.sort();
        assert!(
            !actions.is_empty(),
            "the published Actions are read from {}",
            actions_directory().display()
        );
        actions
            .into_iter()
            .map(|path| {
                let text = std::fs::read_to_string(&path).expect("action document readable");
                let document: serde_yaml::Value =
                    serde_yaml::from_str(&text).expect("action document parses");
                let mut bodies = Vec::new();
                collect_run_bodies(&document, &mut bodies);
                let invocations = bodies
                    .iter()
                    .flat_map(|body| body_invocations(body))
                    .collect();
                (path.display().to_string(), invocations)
            })
            .collect()
    }

    /// Every `intentional` invocation any step of one workflow runs.
    fn invocations(workflow: &str) -> Vec<Vec<String>> {
        let document: serde_yaml::Value =
            serde_yaml::from_str(workflow).expect("derived workflow parses");
        let mut bodies = Vec::new();
        collect_run_bodies(&document, &mut bodies);
        bodies
            .iter()
            .flat_map(|body| body_invocations(body))
            .collect()
    }

    /// Every `intentional` invocation one `run:` body executes.
    ///
    /// Recognition is conservative rather than permissive. Each simple command
    /// the body runs is tokenized the way a shell would, and a fragment that
    /// names the executable but cannot be read as a command fails instead of
    /// passing quietly: a fragment this cannot tokenize, and one that names the
    /// executable somewhere other than command position, are both reported. A
    /// recognizer that shrugs at a shape it does not understand is a hole in
    /// exactly the gate this module exists to be.
    ///
    /// Tokenizing rather than counting raw mentions is what separates a command
    /// from prose about a command. The executable named inside a comment, or
    /// inside a single quoted argument such as an `echo` message, survives
    /// tokenization as one word that is not the executable, so it is neither
    /// classified nor reported. The shell cannot execute it either.
    fn body_invocations(body: &str) -> Vec<Vec<String>> {
        let arrays = array_assignments(body);
        shell_lines(body)
            .iter()
            .flat_map(|line| simple_commands(line))
            .filter_map(|fragment| fragment_invocation(&fragment, &arrays))
            .collect()
    }

    /// The invocation one simple command runs, if it runs this binary.
    ///
    /// Argument arrays the same body assembles are expanded in place, because a
    /// step that collects optional options into an array and expands them into
    /// the command line is still spelling an argument contract this binary has
    /// to accept. Leaving `"${ARGS[@]}"` unexpanded would hand the parser a
    /// command nobody wrote and hide every option the array carries.
    fn fragment_invocation(
        fragment: &str,
        arrays: &BTreeMap<String, Vec<String>>,
    ) -> Option<Vec<String>> {
        let stripped = strip_redirections(fragment);
        let Ok(tokens) = shell_words::split(&stripped) else {
            assert!(
                !names_executable(fragment),
                "`{fragment}` names the intentional executable but cannot be tokenized; an unclassifiable invocation must fail rather than pass"
            );
            return None;
        };
        let tokens = expand_arrays(&tokens, arrays);
        // A command may be preceded by environment assignments; the first word
        // that is not one is the command word.
        let command = tokens.iter().position(|token| !is_assignment(token));
        if command.is_some_and(|index| is_executable(&tokens[index])) {
            return Some(tokens[command.expect("command word")..].to_vec());
        }
        assert!(
            !tokens.iter().any(|token| is_executable(token)),
            "`{fragment}` names the intentional executable outside command position; an unclassifiable invocation must fail rather than pass"
        );
        None
    }

    /// Drop the redirections a shell consumes before the command sees them.
    ///
    /// `intentional verify release-tag >/dev/null` runs a command with no
    /// arguments; handing `>/dev/null` to the parser would reject a command the
    /// shell accepts. The operator also consumes the word that names its
    /// target, wherever the spacing puts it.
    ///
    /// This runs on fragment text rather than on tokens because a shell decides
    /// redirection before it removes quotes, and tokenizing removes them first.
    /// After tokenization `"<extra>"` and `<extra>` are the same word, so a
    /// token-level strip drops a quoted argument the command actually receives
    /// and reports an invocation the step does not run — silently, and green.
    /// The dropped argument is precisely the one the parser would have rejected.
    fn strip_redirections(fragment: &str) -> String {
        let characters = fragment.chars().collect::<Vec<_>>();
        let mut kept = String::new();
        let mut index = 0;
        while index < characters.len() {
            let character = characters[index];
            match character {
                '\'' | '"' => {
                    index = copy_quoted(&characters, index, &mut kept);
                }
                '\\' => {
                    kept.push(character);
                    index += 1;
                    if index < characters.len() {
                        kept.push(characters[index]);
                        index += 1;
                    }
                }
                '<' | '>' => {
                    drop_descriptor(&mut kept);
                    index = skip_redirection(&characters, index);
                }
                _ => {
                    kept.push(character);
                    index += 1;
                }
            }
        }
        kept
    }

    /// Copy one quoted word verbatim, returning the index after its close.
    ///
    /// The quoted span is copied rather than examined because nothing inside it
    /// is an operator. An unterminated quote copies to the end, leaving the
    /// fragment untokenizable so it is reported rather than quietly repaired.
    fn copy_quoted(characters: &[char], start: usize, kept: &mut String) -> usize {
        let quote = characters[start];
        kept.push(quote);
        let mut index = start + 1;
        while index < characters.len() {
            let character = characters[index];
            kept.push(character);
            index += 1;
            if character == '\\' && quote == '"' && index < characters.len() {
                kept.push(characters[index]);
                index += 1;
                continue;
            }
            if character == quote {
                break;
            }
        }
        index
    }

    /// Remove a file-descriptor number the redirection operator that follows owns.
    ///
    /// `2>&1` redirects descriptor two; the digits belong to the operator rather
    /// than to the preceding word. `--count 2 >log` does not, so the digits are
    /// surrendered only when they start a word.
    fn drop_descriptor(kept: &mut String) {
        let digits = kept.chars().rev().take_while(char::is_ascii_digit).count();
        let boundary = kept.len() - digits;
        let starts_a_word = boundary == 0 || kept[..boundary].ends_with(char::is_whitespace);
        if digits > 0 && starts_a_word {
            kept.truncate(boundary);
        }
    }

    /// Skip one redirection operator and the target word it consumes.
    fn skip_redirection(characters: &[char], start: usize) -> usize {
        let mut index = start;
        while index < characters.len() && matches!(characters[index], '<' | '>' | '&') {
            index += 1;
        }
        while index < characters.len() && characters[index].is_whitespace() {
            index += 1;
        }
        let mut quote = None;
        while index < characters.len() {
            let character = characters[index];
            match quote {
                Some(open) => {
                    if character == open {
                        quote = None;
                    }
                }
                None if character == '\'' || character == '"' => quote = Some(character),
                None if character.is_whitespace() => break,
                None => {}
            }
            index += 1;
        }
        index
    }

    /// Whether a command word runs this binary, bare or named by a path.
    ///
    /// An assignment word is excluded because `BIN=/usr/local/bin/intentional`
    /// sets a variable rather than running anything, and classifying it as an
    /// invocation would ask the parser to reject a command nobody wrote.
    fn is_executable(word: &str) -> bool {
        !is_assignment(word) && word.rsplit('/').next() == Some("intentional")
    }

    /// Whether a word assigns a shell variable rather than naming a command.
    fn is_assignment(word: &str) -> bool {
        let Some((name, _)) = word.split_once('=') else {
            return false;
        };
        let name = name.strip_suffix('+').unwrap_or(name);
        !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            && !name.starts_with(|character: char| character.is_ascii_digit())
    }

    /// Whether any whole word of a fragment names the executable.
    ///
    /// Used only to decide whether an untokenizable fragment must be reported,
    /// so it splits on shell punctuation but not on `/`, keeping a
    /// path-qualified mention whole while `intentional_candidate` and
    /// `INTENTIONAL_GLOBAL_TAG` stay distinct words that are not this binary.
    fn names_executable(fragment: &str) -> bool {
        fragment
            .split(|character: char| {
                character.is_whitespace() || "\"'`$(){}[]<>;&|=,".contains(character)
            })
            .any(|word| word.rsplit('/').next() == Some("intentional"))
    }

    /// Substitute every `"${NAME[@]}"` token with the array the body assembled.
    fn expand_arrays(tokens: &[String], arrays: &BTreeMap<String, Vec<String>>) -> Vec<String> {
        tokens
            .iter()
            .flat_map(
                |token| match array_expansion(token).and_then(|name| arrays.get(name)) {
                    Some(elements) => elements.clone(),
                    None => vec![token.clone()],
                },
            )
            .collect()
    }

    /// The array name a token expands, if the token is only that expansion.
    fn array_expansion(token: &str) -> Option<&str> {
        token
            .strip_prefix("${")
            .and_then(|rest| rest.strip_suffix("[@]}"))
            .filter(|name| !name.is_empty())
    }

    /// Every argument array one `run:` body assembles, in source order.
    ///
    /// `NAME=(...)` replaces the array and `NAME+=(...)` extends it, matching
    /// what the shell does, so an array built across conditional branches
    /// contributes every option any branch can pass.
    fn array_assignments(body: &str) -> BTreeMap<String, Vec<String>> {
        let mut arrays: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let lines = body.lines().collect::<Vec<_>>();
        let mut index = 0;
        while index < lines.len() {
            let Some((name, append, first)) = array_assignment_head(lines[index]) else {
                index += 1;
                continue;
            };
            let mut text = first.to_owned();
            while balance(&text) >= 0 && index + 1 < lines.len() {
                index += 1;
                text.push('\n');
                text.push_str(lines[index]);
            }
            let elements = text
                .rsplit_once(')')
                .map(|(interior, _)| interior)
                .and_then(|interior| shell_words::split(interior).ok())
                .unwrap_or_default();
            let entry = arrays.entry(name.to_owned()).or_default();
            if !append {
                entry.clear();
            }
            entry.extend(elements);
            index += 1;
        }
        arrays
    }

    /// The array name, whether it is appended to, and the text after `(`.
    fn array_assignment_head(line: &str) -> Option<(&str, bool, &str)> {
        let (head, rest) = line.trim().split_once("=(")?;
        let append = head.ends_with('+');
        let name = head.strip_suffix('+').unwrap_or(head);
        (!name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_'))
        .then_some((name, append, rest))
    }

    /// How many `(` an array assignment has yet to close.
    fn balance(text: &str) -> isize {
        text.chars().filter(|character| *character == '(').count() as isize
            - text.chars().filter(|character| *character == ')').count() as isize
    }

    /// Split a command line into the simple commands a shell would run.
    ///
    /// Command substitution delimiters separate commands like any operator does,
    /// because the substituted command is itself executed. That is the shape a
    /// `run:` step takes when it projects a command's output into
    /// `$GITHUB_OUTPUT`, so it has to be read rather than stepped over.
    ///
    /// Both subshell parentheses separate. A leading `(` left attached to the
    /// command word would make the fragment start with `(intentional`, which is
    /// neither the executable nor a tokenization failure, so the invocation
    /// would be skipped in silence rather than read or reported.
    fn simple_commands(line: &str) -> Vec<String> {
        let mut work = line.to_owned();
        for separator in ["$(", "&&", "||", "`", "(", ")", "|", ";", "&"] {
            work = work.replace(separator, "\u{0}");
        }
        work.split('\u{0}')
            .map(str::trim)
            .filter(|fragment| !fragment.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Collect the body of every `run:` step anywhere in a workflow document.
    fn collect_run_bodies(value: &serde_yaml::Value, bodies: &mut Vec<String>) {
        match value {
            serde_yaml::Value::Mapping(mapping) => {
                for (key, child) in mapping {
                    if key.as_str() == Some("run") {
                        if let Some(body) = child.as_str() {
                            bodies.push(body.to_owned());
                        }
                    }
                    collect_run_bodies(child, bodies);
                }
            }
            serde_yaml::Value::Sequence(items) => {
                for item in items {
                    collect_run_bodies(item, bodies);
                }
            }
            _ => {}
        }
    }

    /// Split a `run:` body into the command lines a shell executes, rejoining
    /// backslash continuations so a wrapped invocation stays one command.
    fn shell_lines(body: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut pending = String::new();
        for line in body.lines() {
            let trimmed = line.trim();
            if let Some(head) = trimmed.strip_suffix('\\') {
                pending.push_str(head);
                pending.push(' ');
            } else {
                pending.push_str(trimmed);
                lines.push(std::mem::take(&mut pending));
            }
        }
        if !pending.is_empty() {
            lines.push(pending);
        }
        lines
    }

    /// The command path an invocation names, and whether the parser knows all of it.
    ///
    /// The walk stops at the first option because every managed invocation names
    /// its command before any argument, and an unknown segment is reported rather
    /// than dropped so no invocation can pass by being unclassifiable.
    fn command_path(tokens: &[String]) -> (Vec<String>, bool) {
        let root = Cli::command();
        let mut node = &root;
        let mut path = Vec::new();
        for token in tokens.iter().skip(1) {
            if token.starts_with('-') {
                break;
            }
            match node.find_subcommand(token.as_str()) {
                Some(child) => {
                    node = child;
                    path.push(token.clone());
                }
                None if node.has_subcommands() => {
                    path.push(token.clone());
                    return (path, false);
                }
                None => break,
            }
        }
        (path, true)
    }

    /// Reject an invocation this binary would not accept, naming why.
    ///
    /// A shipped Action spells its options literally but takes their values
    /// from workflow inputs, so a value the parser validates against a closed
    /// set arrives as `$INPUT_PUBLISHER` rather than as `npm`. Such a value is
    /// substituted rather than excused: the placeholder is replaced with a value
    /// the parser named as valid and the whole invocation is parsed again. Only
    /// the token clap rejected changes, so every structural defect the same
    /// invocation carries — an unknown option, an unknown subcommand, a missing
    /// required argument, an unexpected positional — is still reached and still
    /// fails.
    ///
    /// Excusing the invocation instead would hide all of them. clap reports one
    /// error per parse, so an Action whose first rejection is a placeholder in a
    /// closed-set option would never have its remaining arguments examined at
    /// all. The option names and the command path are what these documents fix;
    /// their values are supplied at runtime.
    fn parser_rejection(tokens: &[String]) -> Option<String> {
        let mut tokens = tokens.to_vec();
        // Each round resolves one placeholder, and no round introduces one, so
        // the substitution cannot outlast the arguments it is substituting.
        for _ in 0..=tokens.len() {
            let error = Cli::try_parse_from(&tokens).err()?;
            let Some((rejected, accepted)) = placeholder_substitution(&error) else {
                return Some(error.to_string());
            };
            let Some(token) = tokens.iter_mut().find(|token| **token == rejected) else {
                return Some(error.to_string());
            };
            *token = accepted;
        }
        None
    }

    /// Values standing in for a workflow input the parser validates itself.
    ///
    /// An option whose values clap enumerates needs no entry: the parser names
    /// an acceptable value in the rejection itself. An option parsed by a
    /// hand-written conversion carries no such enumeration, so the stand-in is
    /// stated here. Each entry is proved to still be needed, so an option that
    /// stops being closed-set retires its entry rather than lingering as an
    /// unexamined excuse.
    const PLACEHOLDER_VALUES: &[(&str, &str)] = &[("--publisher", "cargo")];

    /// The placeholder value one parse rejected and a value it would accept.
    ///
    /// Only a value-level rejection of an unexpanded shell parameter qualifies.
    /// A rejection this cannot resolve is reported rather than tolerated, so an
    /// Action that introduces a closed-set option fails here until the stand-in
    /// for it is stated, instead of quietly excusing the whole invocation.
    fn placeholder_substitution(error: &clap::error::Error) -> Option<(String, String)> {
        if !matches!(
            error.kind(),
            clap::error::ErrorKind::InvalidValue | clap::error::ErrorKind::ValueValidation
        ) {
            return None;
        }
        let rejected = match error.get(clap::error::ContextKind::InvalidValue)? {
            clap::error::ContextValue::String(value) => value.clone(),
            _ => return None,
        };
        if !rejected.contains('$') {
            return None;
        }
        let accepted = match error.get(clap::error::ContextKind::ValidValue) {
            Some(clap::error::ContextValue::Strings(values)) => values.first().cloned(),
            Some(clap::error::ContextValue::String(value)) => Some(value.clone()),
            _ => rejected_option(error).and_then(stand_in_value),
        }?;
        Some((rejected, accepted))
    }

    /// The long option one rejection names, without its value placeholder.
    fn rejected_option(error: &clap::error::Error) -> Option<String> {
        match error.get(clap::error::ContextKind::InvalidArg)? {
            clap::error::ContextValue::String(argument) => argument
                .split_whitespace()
                .next()
                .filter(|name| name.starts_with("--"))
                .map(str::to_owned),
            _ => None,
        }
    }

    /// The stated stand-in for one option's runtime value.
    fn stand_in_value(option: String) -> Option<String> {
        PLACEHOLDER_VALUES
            .iter()
            .find(|(name, _)| *name == option)
            .map(|(_, value)| (*value).to_owned())
    }

    #[test]
    fn the_parser_accepts_every_generated_invocation() {
        let mut total = 0usize;
        for (role, workflow) in derived_workflows("cli-invocation-gate") {
            for tokens in invocations(&workflow) {
                total += 1;
                let rendered = shell_words::join(&tokens);
                let (path, complete) = command_path(&tokens);
                assert!(
                    complete,
                    "the derived {role} workflow runs `{rendered}`, but `intentional {}` is not a command",
                    path.join(" ")
                );
                Cli::try_parse_from(&tokens).unwrap_or_else(|error| {
                    panic!("the derived {role} workflow runs `{rendered}`, which this binary rejects:\n{error}")
                });
            }
        }
        assert_eq!(
            total, GENERATED_INVOCATIONS,
            "the managed job templates generate a known number of invocations; a template that stopped generating one, or a recognizer that stopped seeing one, must fail here rather than bind fewer commands than it claims"
        );
    }

    #[test]
    fn the_parser_accepts_every_published_action_invocation() {
        let mut total = 0usize;
        for (action, invocations) in action_invocations() {
            for tokens in invocations {
                total += 1;
                let rendered = shell_words::join(&tokens);
                let (path, complete) = command_path(&tokens);
                assert!(
                    complete,
                    "{action} runs `{rendered}`, but `intentional {}` is not a command",
                    path.join(" ")
                );
                if let Some(error) = parser_rejection(&tokens) {
                    panic!("{action} runs `{rendered}`, which this binary rejects:\n{error}");
                }
            }
        }
        assert_eq!(
            total, ACTION_INVOCATIONS,
            "the published Actions invoke a known number of commands; an Action that stopped invoking one, or a recognizer that stopped seeing one, must fail here rather than bind fewer commands than it claims"
        );
    }

    // Parsing what each Action spells today proves the gate reads it; it does
    // not prove the gate would reject a defect. Each Action's own extracted
    // invocation is mutated here and the rejection is asserted per Action, so
    // an Action whose defect the judgement masks fails naming itself rather
    // than hiding inside an aggregate that another Action keeps green.
    #[test]
    fn refuses_a_misspelled_option_in_every_published_action() {
        let mut checked = 0usize;
        for (action, invocations) in action_invocations() {
            for tokens in invocations {
                let option = tokens
                    .iter()
                    .position(|token| token.starts_with("--"))
                    .unwrap_or_else(|| panic!("{action} spells an option to misspell: {tokens:?}"));
                let mut misspelled = tokens.clone();
                misspelled[option] = format!("--x{}", &tokens[option][2..]);
                checked += 1;
                assert!(
                    parser_rejection(&misspelled).is_some(),
                    "{action} would accept `{}`, so a misspelling in it reaches a release runner",
                    shell_words::join(&misspelled)
                );
            }
        }
        assert_eq!(
            checked, ACTION_INVOCATIONS,
            "every published invocation is mutated, not only the ones a defect happens to reach"
        );
    }

    // A required option deleted outright is the same defect class as a
    // misspelled one, and it is the shape that a first-error-wins judgement
    // masks most completely: nothing remains to look wrong.
    #[test]
    fn refuses_a_published_action_that_drops_a_required_option() {
        let (action, invocations) = action_invocations()
            .into_iter()
            .find(|(action, _)| action.contains("verify-publication"))
            .expect("the publication Action is published");
        for tokens in invocations {
            let option = tokens
                .iter()
                .position(|token| token == "--output")
                .expect("the publication Action names the fragment it writes");
            let mut dropped = tokens.clone();
            dropped.drain(option..=option + 1);
            assert!(
                parser_rejection(&dropped).is_some(),
                "{action} would accept `{}`, so a dropped required option reaches a release runner",
                shell_words::join(&dropped)
            );
        }
    }

    // A stand-in that no published Action still needs is an excuse nothing
    // examines. Each entry has to name an option some Action passes a runtime
    // placeholder to, and the stand-in has to be a value this binary accepts,
    // so an option that stops being closed-set retires its entry here.
    #[test]
    fn every_stated_stand_in_is_still_needed_and_still_accepted() {
        let invocations = action_invocations()
            .into_iter()
            .flat_map(|(_, invocations)| invocations)
            .collect::<Vec<_>>();
        for (option, value) in PLACEHOLDER_VALUES {
            let carrier = invocations
                .iter()
                .find(|tokens| {
                    tokens
                        .windows(2)
                        .any(|pair| pair[0] == *option && pair[1].contains('$'))
                })
                .unwrap_or_else(|| {
                    panic!("no published Action passes {option} a runtime placeholder")
                });
            // The entry is load-bearing only while the parser itself names no
            // acceptable value for this option.
            let error = Cli::try_parse_from(carrier).expect_err("the placeholder is rejected");
            assert_eq!(
                placeholder_substitution(&error).map(|(_, accepted)| accepted),
                Some((*value).to_owned()),
                "{option} still needs a stated stand-in rather than naming its own"
            );

            let mut substituted = carrier.clone();
            let index = substituted
                .iter()
                .position(|token| token == option)
                .expect("the option this stand-in names");
            substituted[index + 1] = (*value).to_owned();
            assert!(
                Cli::try_parse_from(&substituted).map_or_else(
                    |error| rejected_option(&error).as_deref() != Some(*option),
                    |_| true
                ),
                "{option} accepts the stated stand-in {value}"
            );
        }
    }

    #[test]
    fn reads_a_subshell_wrapped_invocation() {
        let invocations = body_invocations("(intentional release prepare --output /tmp/candidate)");
        assert_eq!(
            invocations.len(),
            1,
            "a subshell runs the command it wraps: {invocations:?}"
        );
        assert_eq!(
            invocations[0][1..],
            ["release", "prepare", "--output", "/tmp/candidate"]
        );
    }

    #[test]
    fn reads_a_redirected_invocation_as_the_command_the_shell_runs() {
        for body in [
            "intentional verify release-tag >/dev/null 2>&1",
            "intentional verify release-tag > /dev/null",
        ] {
            let invocations = body_invocations(body);
            assert_eq!(invocations.len(), 1, "`{body}`: {invocations:?}");
            assert_eq!(invocations[0][1..], ["verify", "release-tag"], "`{body}`");
            assert!(
                parser_rejection(&invocations[0]).is_none(),
                "`{body}` is a command this binary accepts"
            );
        }
    }

    /// A quoted word a redirection operator only resembles is still an argument.
    ///
    /// The shell decides redirection before quote removal, so `"<extra>"` is a
    /// word the command receives rather than a redirection the shell consumes.
    /// A gate that drops it reports a command the step does not run, and the
    /// argument it dropped is exactly the one the parser would have rejected.
    #[test]
    fn keeps_a_quoted_word_a_redirection_operator_only_resembles() {
        for (body, argument) in [
            ("intentional verify release-tag \"<extra>\"", "<extra>"),
            ("intentional verify release-tag '>report'", ">report"),
            ("intentional verify release-tag \"<\"", "<"),
            ("intentional verify release-tag \">>out\"", ">>out"),
        ] {
            let invocations = body_invocations(body);
            assert_eq!(invocations.len(), 1, "`{body}`: {invocations:?}");
            assert_eq!(
                invocations[0][1..],
                ["verify", "release-tag", argument],
                "`{body}` passes a quoted word the shell does not read as a redirection"
            );
            assert!(
                parser_rejection(&invocations[0]).is_some(),
                "`{body}` passes an argument this binary rejects"
            );
        }
    }

    /// A quoted word carrying a command separator is reported, not read.
    ///
    /// Simple commands are split on separators without regard to quoting, so a
    /// quoted `&`, `|`, or `;` cuts the fragment before redirection stripping
    /// sees it. That leaves a word this cannot tokenize, and the recognizer
    /// refuses it rather than classifying half a command line. The boundary is
    /// stated here so it stays a loud refusal rather than becoming a silent
    /// acceptance the way the quoted redirection did.
    #[test]
    #[should_panic(expected = "unclassifiable invocation must fail rather than pass")]
    fn refuses_a_quoted_word_carrying_a_command_separator() {
        body_invocations("intentional verify release-tag \"2>&1\"");
    }

    /// Redirection is still consumed when the shell would consume it.
    #[test]
    fn drops_a_redirection_whose_target_is_quoted() {
        for body in [
            "intentional verify release-tag > \"/dev/null\"",
            "intentional verify release-tag >'/dev/null'",
        ] {
            let invocations = body_invocations(body);
            assert_eq!(invocations.len(), 1, "`{body}`: {invocations:?}");
            assert_eq!(invocations[0][1..], ["verify", "release-tag"], "`{body}`");
        }
    }

    /// A digit belongs to the operator only when it starts the redirection word.
    ///
    /// `2>log` names a descriptor the operator owns; `--phase 2 >log` ends in an
    /// argument the command receives. Surrendering the second would drop a value
    /// the parser validates.
    #[test]
    fn surrenders_a_descriptor_without_surrendering_a_numeric_argument() {
        let descriptor = body_invocations("intentional verify release-tag 2>log");
        assert_eq!(descriptor.len(), 1, "{descriptor:?}");
        assert_eq!(descriptor[0][1..], ["verify", "release-tag"]);

        let argument = body_invocations("intentional verify release-tag 2 >log");
        assert_eq!(argument.len(), 1, "{argument:?}");
        assert_eq!(
            argument[0][1..],
            ["verify", "release-tag", "2"],
            "a numeric argument is not a file descriptor"
        );

        // Digits that end a longer word are part of that word, however tightly
        // the redirection follows. Surrendering them would truncate the value
        // reaching the parser rather than drop a descriptor.
        let attached = body_invocations("intentional evidence contribute --namespace ns2>log");
        assert_eq!(attached.len(), 1, "{attached:?}");
        assert_eq!(
            attached[0][1..],
            ["evidence", "contribute", "--namespace", "ns2"],
            "a word ending in digits keeps them"
        );
    }

    #[test]
    fn reads_the_invocation_however_a_step_body_spells_it() {
        for body in [
            "intentional release prepare --output /tmp/candidate",
            "/usr/local/bin/intentional release prepare --output /tmp/candidate",
            "echo \"sha=$(intentional release prepare --output /tmp/candidate)\" >> $GITHUB_OUTPUT",
            "intentional release prepare --output /tmp/candidate | tee /tmp/log",
            "set -euo pipefail\nintentional release prepare \\\n  --output /tmp/candidate",
        ] {
            let invocations = body_invocations(body);
            assert_eq!(
                invocations.len(),
                1,
                "`{body}` runs one command: {invocations:?}"
            );
            assert_eq!(
                invocations[0][1..],
                ["release", "prepare", "--output", "/tmp/candidate"],
                "`{body}`"
            );
        }
    }

    #[test]
    fn ignores_a_name_that_only_begins_with_the_executable() {
        let body = "gh release upload \"${INTENTIONAL_GLOBAL_TAG}\" \"${{ runner.temp }}/intentional_release\"/* --clobber";
        assert!(body_invocations(body).is_empty(), "{body}");
    }

    #[test]
    #[should_panic(expected = "unclassifiable invocation must fail rather than pass")]
    fn refuses_a_command_line_it_cannot_read() {
        body_invocations("intentional release prepare --output \"/tmp/unterminated");
    }

    /// A backslash-escaped redirection operator is an argument, not an operator.
    ///
    /// The escape survives to `strip_redirections` intact: `shell_lines` rejoins
    /// only a backslash at end of line, and `simple_commands` splits on
    /// `$( && || ` ( ) | ; &` — none of which this fragment carries. So the
    /// backslash arm is the only thing standing between `a\>b` and the `'<' |
    /// '>'` arm, which would drop `>b` as a redirection and hand the parser an
    /// argument the shell never passes.
    #[test]
    fn keeps_an_escaped_redirection_operator_inside_an_argument() {
        let invocations =
            body_invocations("intentional release prepare --output /tmp/a\\>b --format json");
        assert_eq!(invocations.len(), 1, "{invocations:?}");
        assert_eq!(
            invocations[0][1..],
            ["release", "prepare", "--output", "/tmp/a>b", "--format", "json"],
            "the shell passes the escaped operator as part of the argument"
        );
    }

    /// A backslash-escaped quote does not close the quoted span it sits in.
    ///
    /// The escape survives to `copy_quoted` intact: `shell_lines` rejoins only a
    /// trailing backslash, and `simple_commands` carries no rule for `\` or `"`.
    /// Without the escaped-quote arm the span closes at `\"`, the `>` that
    /// follows is read as a redirection operator, and the fragment stops being
    /// tokenizable at all.
    #[test]
    fn keeps_an_escaped_quote_from_closing_its_span() {
        let invocations = body_invocations(
            "intentional release prepare --output \"/tmp/a\\\"b>c\" --format json",
        );
        assert_eq!(invocations.len(), 1, "{invocations:?}");
        assert_eq!(
            invocations[0][1..],
            [
                "release",
                "prepare",
                "--output",
                "/tmp/a\"b>c",
                "--format",
                "json"
            ],
            "the quoted span runs to its unescaped close, redirection and all"
        );
    }

    #[test]
    #[should_panic(expected = "outside command position")]
    fn refuses_an_invocation_reached_through_another_command() {
        body_invocations("xargs intentional release prepare --output /tmp/candidate");
    }

    #[test]
    fn reads_prose_about_the_command_as_prose() {
        for body in [
            "# intentional release prepare --output /tmp/candidate",
            "echo \"::error::intentional evidence contribute did not name one artifact.\"",
            "echo 'run intentional release prepare first'",
        ] {
            assert!(
                body_invocations(body).is_empty(),
                "`{body}` mentions the command without running it"
            );
        }
    }

    #[test]
    fn reads_an_assignment_of_the_binary_path_as_an_assignment() {
        assert!(
            body_invocations("BIN=/usr/local/bin/intentional").is_empty(),
            "an assignment word names a path rather than running it"
        );
        let prefixed = body_invocations("INTENTIONAL_LOG=debug intentional verify release-tag");
        assert_eq!(
            prefixed.len(),
            1,
            "an environment prefix still runs the command: {prefixed:?}"
        );
        assert_eq!(prefixed[0][1..], ["verify", "release-tag"]);
    }

    #[test]
    fn expands_an_argument_array_the_same_body_assembled() {
        let body = "ARGS=(--namespace ns\n  --output /tmp/bundle)\n\
                    if [[ -n \"$VALUE\" ]]; then\n  ARGS+=(--value-file \"$VALUE\")\nfi\n\
                    RESULT=\"$(intentional evidence contribute \"${ARGS[@]}\")\"";
        let invocations = body_invocations(body);
        assert_eq!(invocations.len(), 1, "{invocations:?}");
        assert_eq!(
            invocations[0][1..],
            [
                "evidence",
                "contribute",
                "--namespace",
                "ns",
                "--output",
                "/tmp/bundle",
                "--value-file",
                "$VALUE"
            ],
            "every option the array can carry reaches the parser"
        );
    }
}
