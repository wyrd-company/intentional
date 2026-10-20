// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use intentional_core::{
    check_workspace, initialize, ApplyResult, Bump, Config, InitState, IntentDraft, ReleasePlan,
    Severity, StampResult, TagPhase, TagResult, WorkspaceStatus, CONFIG_PATH,
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
    /// Create annotated tag records or establish baseline tags.
    Tag(TagArgs),
    /// Validate config, intents, and deterministic planning for CI.
    Check,
    /// Verify one established Release or baseline from its annotated tag.
    Verify(VerifyArgs),
    /// Print the agent-facing Intentional workflow skill.
    Skill,
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
struct VerifyArgs {
    /// Exact annotated tag that identifies the Release or Release Unit.
    tag: String,

    /// Emit stable structured verification results.
    #[arg(long)]
    json: bool,
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
        Command::Verify(args) => return verify(&cli.directory, args),
        Command::Add(args) => add(&cli.directory, args),
        Command::Status => status(&cli.directory),
        Command::Plan(args) => plan(&cli.directory, args.channel.as_deref()),
        Command::Apply(args) => apply(&cli.directory, args.channel.as_deref(), args.dry_run),
        Command::Stamp(args) => stamp(&cli.directory, args.prerelease.as_deref(), args.dry_run),
        Command::Tag(args) => tag(&cli.directory, args),
        Command::Check => check(&cli.directory),
        Command::Skill => skill(),
    }?;
    Ok(0)
}

fn verify(root: &std::path::Path, args: VerifyArgs) -> Result<u8> {
    let verification = intentional_core::verify_release(root, &args.tag)?;
    if args.json {
        println!("{}", verification.to_json()?);
    } else {
        match &verification.target {
            Some(target) if verification.verified => {
                println!("verified {} at {target}", verification.tag)
            }
            _ => println!("verification failed for {}", verification.tag),
        }
        for guarantee in &verification.guarantees {
            println!("guarantee: {guarantee}");
        }
        for tag in &verification.identity_tags {
            println!("identity tag: {tag}");
        }
        for diagnostic in &verification.diagnostics {
            let severity = match diagnostic.severity {
                Severity::Error => "error",
                Severity::Advisory => "advisory",
            };
            println!("{severity} [{}] {}", diagnostic.code, diagnostic.message);
        }
    }
    Ok(if verification.verified { 0 } else { 1 })
}

fn skill() -> Result<()> {
    print!("{SKILL_DOCUMENT}");
    Ok(())
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
    println!("{}", plan.encode()?);
    Ok(())
}

fn init(root: &std::path::Path, args: InitArgs) -> Result<u8> {
    let result = initialize(root, args.take_over)?;
    if args.json {
        println!("{}", result.to_json()?);
    } else {
        println!("initialization state: {:?}", result.state);
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

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;
    use std::collections::BTreeSet;

    /// Commands the specification states ahead of their implementation.
    const SPECIFIED_AHEAD: [&str; 1] = ["model"];

    fn long_flags(command: &clap::Command) -> BTreeSet<String> {
        command
            .get_arguments()
            .filter(|argument| !argument.is_global_set())
            .filter(|argument| !matches!(argument.get_id().as_str(), "help" | "version"))
            .filter_map(|argument| argument.get_long())
            .map(|long| format!("--{long}"))
            .collect()
    }

    fn specified_flags(options: &serde_yaml::Value) -> BTreeSet<String> {
        options
            .as_sequence()
            .map(|options| {
                options
                    .iter()
                    .flat_map(|option| option["flags"].as_sequence().cloned().unwrap_or_default())
                    .filter_map(|flag| flag.as_str().map(str::to_owned))
                    .filter(|flag| flag.starts_with("--"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn commands_and_flags_match_the_cli_specification() {
        let specification: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../../docs/specifications/command-line-interface.spec.yml"
        ))
        .expect("CLI specification parses");
        let cli = Cli::command();
        let shipped = cli
            .get_subcommands()
            .map(|command| command.get_name().to_owned())
            .collect::<BTreeSet<_>>();
        let specified = specification["commands"]
            .as_sequence()
            .expect("commands")
            .iter()
            .filter_map(|command| command["name"].as_str().map(str::to_owned))
            .filter(|name| !SPECIFIED_AHEAD.contains(&name.as_str()))
            .collect::<BTreeSet<_>>();
        assert_eq!(shipped, specified);
        for command in specification["commands"].as_sequence().expect("commands") {
            let name = command["name"].as_str().expect("command name");
            let Some(subcommand) = cli.find_subcommand(name) else {
                continue;
            };
            assert_eq!(
                long_flags(subcommand),
                specified_flags(&command["options"]),
                "{name}"
            );
        }
        assert_eq!(
            specified_flags(&specification["global-options"]),
            BTreeSet::from(["--directory".to_owned()])
        );
    }
}
