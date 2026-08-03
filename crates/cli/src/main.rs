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
    CONFIG_PATH, LOCAL_JOB, MISSING_BASELINE_CODE, MISSING_BASELINE_NEXT_ACTION,
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
/// The coverage is the `run:` bodies of derived workflows and nothing else. The
/// composite Actions this repository publishes under `actions/` invoke the same
/// portable commands from their own `run:` bodies and are not read here, so
/// those invocations agree with the parser by inspection rather than by gate.
#[cfg(test)]
mod generated_invocations {
    use super::*;
    use clap::CommandFactory;
    use std::collections::BTreeSet;
    use std::path::Path;

    /// Commands the release protocol specifies that this runtime has not implemented.
    ///
    /// A generated invocation of one of these cannot be parsed yet, so it passes
    /// by declaration rather than by treating an unrecognized command as a skip.
    /// The allowance retires itself from both directions: an entry the parser now
    /// accepts fails `every_pending_command_is_still_absent`, and an entry no
    /// generated workflow reaches fails
    /// `the_parser_accepts_every_generated_invocation`. Whoever implements one of
    /// these commands must therefore validate the invocation the workflow
    /// generates for it instead of inheriting an unchecked one.
    const PENDING_COMMANDS: &[&[&str]] = &[&["verify", "publication"], &["verify", "release-tag"]];

    /// Invocations the managed job templates generate for the fixture workspace.
    ///
    /// The release role generates `intentional release prepare` and
    /// `intentional verify handoff`. The publish role generates
    /// `intentional verify release-tag`, one `intentional verify publication`
    /// per selected publication, and `intentional evidence assemble`; the
    /// fixture configures exactly one publication.
    ///
    /// Parsing what was extracted proves nothing about what was missed, so the
    /// count is asserted rather than assumed. A template that stops generating a
    /// command, and a recognizer that stops seeing one, both fail here and force
    /// a deliberate update instead of quietly binding a smaller surface.
    const GENERATED_INVOCATIONS: usize = 5;

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    cargo: {}
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
"#;

    const COMPONENT_MANIFEST: &str =
        "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n";

    const RELEASE_WORKFLOW: &str =
        "name: release\n\non:\n  workflow_dispatch:\n\njobs:\n  repository_job:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n";

    const PUBLISH_WORKFLOW: &str =
        "name: publish\n\non:\n  push:\n    tags:\n      - 'legacy-*'\n\njobs:\n  repository_job:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n";

    /// Reconcile a representative workspace and return what each role's workflow became.
    fn derived_workflows() -> Vec<(WorkflowRole, String)> {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let root = workspace.path();
        write(root, ".intentional/config.yml", CONFIG);
        write(root, "component/Cargo.toml", COMPONENT_MANIFEST);
        write(root, ".github/workflows/release.yml", RELEASE_WORKFLOW);
        write(root, ".github/workflows/publish.yml", PUBLISH_WORKFLOW);

        WorkflowRole::ALL
            .into_iter()
            .map(|role| {
                let comparison = compare_workflow(root, role, None).expect("comparison runs");
                assert_eq!(
                    comparison.status,
                    ComparisonStatus::Different,
                    "the {role} contract must derive: {:?}",
                    comparison.diagnostics
                );
                let applied = comparison.apply().expect("transformation applies");
                assert!(applied.applied);
                let derived =
                    std::fs::read_to_string(root.join(&applied.path)).expect("derived workflow");
                (role, derived)
            })
            .collect()
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("fixture directory");
        std::fs::write(path, contents).expect("fixture file");
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
    /// Recognition is conservative rather than permissive. Each command line is
    /// counted for mentions of the executable first, and a line whose mentions
    /// are not all classified as invocations fails instead of passing quietly.
    /// A step that wraps the command in a substitution, names it by an absolute
    /// path, or quotes it in a way this cannot tokenize is therefore reported,
    /// because a recognizer that shrugs at a shape it does not understand is a
    /// hole in exactly the gate this module exists to be.
    fn body_invocations(body: &str) -> Vec<Vec<String>> {
        let mut invocations = Vec::new();
        for line in shell_lines(body) {
            let mentions = executable_mentions(&line);
            if mentions == 0 {
                continue;
            }
            let classified = line_invocations(&line);
            assert_eq!(
                classified.len(),
                mentions,
                "`{line}` names the intentional executable {mentions} time(s) but {} of them could be read as a command; an unclassifiable invocation must fail rather than pass",
                classified.len()
            );
            invocations.extend(classified);
        }
        invocations
    }

    /// The `intentional` invocations one command line runs in command position.
    fn line_invocations(line: &str) -> Vec<Vec<String>> {
        simple_commands(line)
            .iter()
            .filter_map(|fragment| shell_words::split(fragment).ok())
            .filter(|tokens| tokens.first().is_some_and(|token| is_executable(token)))
            .collect()
    }

    /// Whether a command word runs this binary, bare or named by a path.
    fn is_executable(word: &str) -> bool {
        word.rsplit('/').next() == Some("intentional")
    }

    /// Count every mention of the executable in a command line, wherever it sits.
    ///
    /// Splitting on shell punctuation but not on `/` keeps a path-qualified
    /// mention whole and leaves a mention inside a command substitution or an
    /// assignment visible, while `intentional_candidate` and
    /// `INTENTIONAL_GLOBAL_TAG` stay distinct words that are not this binary.
    fn executable_mentions(line: &str) -> usize {
        line.split(|character: char| {
            character.is_whitespace() || "\"'`$(){}[]<>;&|=,".contains(character)
        })
        .filter(|word| is_executable(word))
        .count()
    }

    /// Split a command line into the simple commands a shell would run.
    ///
    /// Command substitution delimiters separate commands like any operator does,
    /// because the substituted command is itself executed. That is the shape a
    /// `run:` step takes when it projects a command's output into
    /// `$GITHUB_OUTPUT`, so it has to be read rather than stepped over.
    fn simple_commands(line: &str) -> Vec<String> {
        let mut work = line.to_owned();
        for separator in ["$(", "&&", "||", "`", ")", "|", ";", "&"] {
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

    fn names(command: &[&str], path: &[String]) -> bool {
        command.len() == path.len()
            && command
                .iter()
                .zip(path)
                .all(|(declared, observed)| *declared == observed.as_str())
    }

    fn declared_pending() -> BTreeSet<Vec<String>> {
        PENDING_COMMANDS
            .iter()
            .map(|command| command.iter().map(|&part| part.to_owned()).collect())
            .collect()
    }

    #[test]
    fn the_parser_accepts_every_generated_invocation() {
        let mut reached = BTreeSet::new();
        let mut total = 0usize;
        for (role, workflow) in derived_workflows() {
            for tokens in invocations(&workflow) {
                total += 1;
                let rendered = shell_words::join(&tokens);
                let (path, complete) = command_path(&tokens);
                if complete {
                    Cli::try_parse_from(&tokens).unwrap_or_else(|error| {
                        panic!("the derived {role} workflow runs `{rendered}`, which this binary rejects:\n{error}")
                    });
                    continue;
                }
                assert!(
                    PENDING_COMMANDS.iter().any(|command| names(command, &path)),
                    "the derived {role} workflow runs `{rendered}`, but `intentional {}` is not a command and is not declared pending",
                    path.join(" ")
                );
                reached.insert(path);
            }
        }
        assert_eq!(
            total, GENERATED_INVOCATIONS,
            "the managed job templates generate a known number of invocations; a template that stopped generating one, or a recognizer that stopped seeing one, must fail here rather than bind fewer commands than it claims"
        );
        assert_eq!(
            reached,
            declared_pending(),
            "every pending command must still be reached by a generated invocation"
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

    #[test]
    fn every_pending_command_is_still_absent() {
        let root = Cli::command();
        for command in PENDING_COMMANDS {
            let mut node = Some(&root);
            for segment in *command {
                node = node.and_then(|current| current.find_subcommand(*segment));
            }
            assert!(
                node.is_none(),
                "`intentional {}` is now a command; validate the invocation the workflow generates for it and remove it from PENDING_COMMANDS",
                command.join(" ")
            );
        }
    }
}
