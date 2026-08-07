// ---
// relationships:
//   implements: github-release-executor
// ---

//! Syntax gates for the workflows Intentional derives.
//!
//! This crate already enables core's `test-support` feature for its generated
//! invocation tests. Keeping the external-tool gate on that same test boundary
//! reuses the fixture without exposing it in a release build.

use intentional_core::config::WorkflowRole;
use intentional_core::executor::{
    fixture::{
        derived_recipe_workflows, derived_recipes, prefixed_derived_recipe_workflows,
        underived_recipes,
    },
    OWNERSHIP_SENTINEL,
};
use serde_yaml::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Collect every `run:` body from a parsed workflow without naming its jobs.
fn collect_run_bodies(value: &Value, bodies: &mut Vec<String>) {
    match value {
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                if key.as_str() == Some("run") {
                    if let Some(body) = child.as_str() {
                        bodies.push(body.to_owned());
                    }
                }
                collect_run_bodies(child, bodies);
            }
        }
        Value::Sequence(items) => {
            for item in items {
                collect_run_bodies(item, bodies);
            }
        }
        _ => {}
    }
}

/// Count authored `run:` keys independently of the YAML tree walk.
fn textual_run_count(workflow: &str) -> usize {
    workflow
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("run:") || line.starts_with("- run:")
        })
        .count()
}

/// External parser and the generated surface it owns.
#[derive(Clone, Copy)]
enum SyntaxTool {
    Actionlint,
    Shellcheck,
}

impl SyntaxTool {
    const ALL: [Self; 2] = [Self::Actionlint, Self::Shellcheck];

    const fn command(self) -> &'static str {
        match self {
            Self::Actionlint => "actionlint",
            Self::Shellcheck => "shellcheck",
        }
    }

    fn executable(self) -> OsString {
        std::env::var_os(self.command().to_ascii_uppercase())
            .unwrap_or_else(|| self.command().into())
    }

    const fn unchecked(self) -> &'static str {
        match self {
            Self::Actionlint => "derived workflow syntax",
            Self::Shellcheck => "derived run-body shell syntax",
        }
    }

    fn unavailable(self, error: std::io::Error) -> String {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "{} is missing; {} was not checked",
                self.command(),
                self.unchecked()
            )
        } else {
            format!("cannot run {}: {error}", self.command())
        }
    }
}

fn actionlint(path: &std::path::Path) -> Result<Output, String> {
    Command::new(SyntaxTool::Actionlint.executable())
        // Shell bodies are checked one by one below, so this invocation owns
        // workflow syntax and cannot mask a skipped body extraction.
        .arg("-shellcheck=")
        .arg(path)
        .output()
        .map_err(|error| SyntaxTool::Actionlint.unavailable(error))
}

fn shellcheck(body: &str) -> Result<Output, String> {
    let mut child = Command::new(SyntaxTool::Shellcheck.executable())
        .args(["--shell=bash", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| SyntaxTool::Shellcheck.unavailable(error))?;
    child
        .stdin
        .take()
        .expect("shellcheck stdin")
        .write_all(body.as_bytes())
        .expect("write shell body");
    child
        .wait_with_output()
        .map_err(|error| format!("cannot finish shellcheck: {error}"))
}

fn command_runs_rust_tests(command: &str) -> bool {
    command.lines().any(|line| {
        let Ok(words) = shell_words::split(line) else {
            return false;
        };
        words.iter().enumerate().any(|(position, word)| {
            matches!(word.as_str(), "cargo" | "cross")
                && words[position + 1..]
                    .iter()
                    .any(|argument| matches!(argument.as_str(), "test" | "nextest"))
        })
    })
}

fn task_reaches_rust_tests(name: &str, tasks: &Value, visiting: &mut BTreeSet<String>) -> bool {
    if !visiting.insert(name.to_owned()) {
        return false;
    }
    let task = &tasks[name];
    let dependency_reaches = task["deps"].as_sequence().is_some_and(|dependencies| {
        dependencies.iter().any(|dependency| {
            dependency
                .as_str()
                .is_some_and(|dependency| task_reaches_rust_tests(dependency, tasks, visiting))
        })
    });
    let command_reaches = task["cmds"].as_sequence().is_some_and(|commands| {
        commands.iter().any(|command| {
            let command = command
                .as_str()
                .or_else(|| command["cmd"].as_str())
                .unwrap_or_default();
            command_runs_rust_tests(command) || command_invokes_test_task(command, tasks, visiting)
        })
    });
    visiting.remove(name);
    dependency_reaches || command_reaches
}

fn command_invokes_test_task(
    command: &str,
    tasks: &Value,
    visiting: &mut BTreeSet<String>,
) -> bool {
    command.lines().any(|line| {
        let Ok(words) = shell_words::split(line) else {
            return false;
        };
        words.iter().enumerate().any(|(position, word)| {
            word == "task"
                && words[position + 1..].iter().any(|argument| {
                    !argument.starts_with('-')
                        && !tasks[argument.as_str()].is_null()
                        && task_reaches_rust_tests(argument, tasks, visiting)
                })
        })
    })
}

fn command_reaches_rust_tests(command: &str, tasks: &Value) -> bool {
    command_runs_rust_tests(command)
        || command_invokes_test_task(command, tasks, &mut BTreeSet::new())
}

#[test]
fn every_required_tool_names_the_surface_its_absence_leaves_unchecked() {
    let missing = || std::io::Error::from(std::io::ErrorKind::NotFound);
    let messages = SyntaxTool::ALL
        .map(|tool| tool.unavailable(missing()))
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        [
            "actionlint is missing; derived workflow syntax was not checked",
            "shellcheck is missing; derived run-body shell syntax was not checked",
        ]
    );
}

#[test]
fn every_hosted_job_running_workspace_tests_installs_the_tools_the_suite_requires() {
    let directory = std::path::Path::new("../../.github/workflows");
    let taskfile = std::fs::read_to_string("../../Taskfile.yml").expect("Taskfile is readable");
    let taskfile: Value = serde_yaml::from_str(&taskfile).expect("Taskfile parses");
    let tasks = &taskfile["tasks"];
    let baseline = std::fs::read_to_string("../../scripts/release/linux-gnu-baseline.env")
        .expect("GNU baseline is readable");
    assert!(
        baseline.lines().any(|line| {
            line == "GNU_CROSS_TEST_ARGUMENTS=\"--workspace --locked --all-targets --no-fail-fast --target x86_64-unknown-linux-gnu\""
        }),
        "the shared GNU test arguments run every target without hiding later failures"
    );
    for command in [
        "cargo test --all",
        "cargo nextest run --workspace",
        "cross --verbose test --workspace",
        "task test",
        "task ci",
    ] {
        assert!(
            command_reaches_rust_tests(command, tasks),
            "{command} reaches Rust tests"
        );
    }
    for command in ["cargo clippy --all-targets", "npm test", "test-retry.sh"] {
        assert!(
            !command_reaches_rust_tests(command, tasks),
            "{command} does not run Rust tests"
        );
    }
    let mut reached = Vec::new();
    for entry in std::fs::read_dir(directory).expect("workflow directory is readable") {
        let path = entry.expect("workflow entry is readable").path();
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let workflow = std::fs::read_to_string(&path).expect("workflow is readable");
        let document: Value = serde_yaml::from_str(&workflow).expect("workflow parses");
        for (job, body) in document["jobs"]
            .as_mapping()
            .expect("workflow declares jobs")
        {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            for (test_position, test) in steps.iter().enumerate() {
                let Some(command) = test["run"].as_str() else {
                    continue;
                };
                if !command_reaches_rust_tests(command, tasks) {
                    continue;
                }
                let cross = command.lines().any(|line| {
                    shell_words::split(line)
                        .is_ok_and(|words| words.iter().any(|word| word == "cross"))
                });
                let job = job.as_str().expect("job id");
                let installation = steps[..test_position]
                    .iter()
                    .rev()
                    .find(|step| {
                        step["run"].as_str().is_some_and(|run| {
                            run.contains("scripts/ci/install-workflow-test-tools.sh .ci-tools/bin")
                        })
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{} job {job} installs workflow test tools before its suite",
                            path.display()
                        )
                    });
                assert_eq!(
                    installation["if"],
                    test["if"],
                    "{} job {job} installs tools under the same condition that runs tests",
                    path.display()
                );
                if cross {
                    let installation = installation["run"].as_str().expect("installation command");
                    assert!(
                        installation.contains(
                            "cat .ci-tools/bin/workflow-test-tools.env >> \"$GITHUB_ENV\""
                        ),
                        "{} job {job} exports the installer-derived Cross environment",
                        path.display()
                    );
                    assert_eq!(
                        command,
            "# Shared baseline is an argument list.\n# shellcheck disable=SC2086\ncross test $GNU_CROSS_TEST_ARGUMENTS\n",
                        "{} job {job} uses the shared complete-suite argument contract",
                        path.display()
                    );
                } else {
                    assert!(
                        installation["run"]
                            .as_str()
                            .expect("installation command")
                            .contains(".ci-tools/bin\" >> \"$GITHUB_PATH"),
                        "{} job {job} exposes installed tools to native tests",
                        path.display()
                    );
                }
                reached.push(format!("{}:{job}", path.display()));
            }
        }
    }
    reached.sort();
    assert_eq!(
        reached,
        [
            "../../.github/workflows/cd.yml:build",
            "../../.github/workflows/ci.yml:minimum-rust",
            "../../.github/workflows/ci.yml:repository-ci",
            "../../.github/workflows/ci.yml:test",
            "../../.github/workflows/ci.yml:test",
            "../../.github/workflows/linux-gnu-evidence.yml:native-arm64",
            "../../.github/workflows/linux-gnu-evidence.yml:test-x86_64-gnu-toolchain",
        ],
        "the derived workspace-test command population"
    );
}

#[test]
fn hosted_gate_queries_every_check_for_the_selected_pull_request() {
    let taskfile = std::fs::read_to_string("../../Taskfile.yml").expect("Taskfile is readable");
    let document: Value = serde_yaml::from_str(&taskfile).expect("Taskfile parses");
    let gate = &document["tasks"]["hosted:check"];
    assert_eq!(
        gate["requires"]["vars"].as_sequence(),
        Some(&vec![Value::String("PR".to_owned())]),
        "hosted status requires an explicit pull request"
    );
    assert_eq!(
        gate["cmds"][0].as_str(),
        Some("gh pr checks {{ .PR }}"),
        "hosted status reads every check instead of trusting the local gate set"
    );
}

/// Catalog recipes that derive no steps, and so are covered by a refusal.
///
/// Empty, and that is the assertion. Every maintained recipe derives steps, so
/// `derived_workflows_and_their_shell_bodies_parse` reaching exactly
/// `derived_recipes()` covers the whole catalog and nothing is left over.
///
/// The refusal path is still live and still proved — by
/// `refuses_a_publication_whose_maintained_recipe_is_not_derived`, against a
/// synthetic packager and publisher pair rather than a catalog member, because
/// no catalog member exercises it any more.
///
/// A recipe added without steps, or one that stops deriving, appears in
/// `underived_recipes()` and fails the equality below. Refusing it is then not
/// enough: it also leaves the reach assertion, so it is covered by nothing until
/// someone proves its refusal and lists it here.
const REFUSED_RECIPES: &[(&str, &str)] = &[];

/// Every catalog recipe is reached by the derived shell, or has a refusal.
///
/// `derived_workflows_and_their_shell_bodies_parse` asserts the derived shell
/// reaches exactly `derived_recipes()`. Nothing asserted anything about the
/// complement, so a recipe excluded from derivation sat outside both sides of
/// that equality and was invisible: it could be added, or silently stop
/// deriving, and no test would change colour.
///
/// **What is asserted is the equality below, and only that.** Comparing the two
/// set *sizes* against the catalog would prove nothing —`underived_recipes()` is
/// the same catalog under the negation of the same predicate, so their lengths
/// sum to the catalog for any predicate whatsoever, including a broken one. That
/// sum is a fact about two function bodies, not about derivation, and it is left
/// out rather than written down where a later reader could mistake it for
/// coverage.
#[test]
fn every_catalog_recipe_is_either_derived_or_refused() {
    let underived = underived_recipes();

    let refused = underived
        .iter()
        .map(|recipe| {
            (
                recipe.packager.as_str().to_owned(),
                recipe.publisher.as_str().to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    let rostered = REFUSED_RECIPES
        .iter()
        .map(|(packager, publisher)| ((*packager).to_owned(), (*publisher).to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        refused, rostered,
        "a catalog recipe changed which side of derivation it falls on. Every underived \
         recipe needs a refusal proved in \
         refuses_a_publication_whose_maintained_recipe_is_not_derived and listed in \
         REFUSED_RECIPES; a recipe that starts deriving must leave both."
    );
}

/// The job that builds the release candidate mints no repository token.
///
/// This is the load-bearing half of the authority split. The preparation job
/// runs before anything has verified the candidate it produces, so a repository
/// token there is authority granted ahead of the check that authorises it —
/// which is the whole reason the work is split across two jobs.
///
/// It needs its own assertion, and the reason is worth stating because it is not
/// obvious. `resolves_every_intentional_action_before_any_credential_is_minted`
/// looks like it covers this: it finds the mint step and requires every
/// Intentional Action to precede it. But it locates the mint *first* and
/// quantifies over steps second, so a mint appended after the last Intentional
/// Action satisfies it for every step and passes **vacuously**. It constrains
/// where a mint sits relative to Action resolution; it does not constrain
/// whether this job mints at all.
///
/// The job is identified by the Action that gives it its purpose rather than by
/// name or position, so a rename or a reordering does not quietly empty the
/// check.
#[test]
fn the_release_preparation_job_mints_no_repository_token() {
    let workflows = derived_recipe_workflows("derived-workflow-prepare-authority");
    let mut preparation_jobs = 0usize;

    for (role, workflow) in &workflows {
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let jobs = document["jobs"]
            .as_mapping()
            .expect("derived workflow jobs");
        for (job, body) in jobs {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            let builds_candidate = steps.iter().any(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.contains("/actions/prepare-release@"))
            });
            if !builds_candidate {
                continue;
            }
            preparation_jobs += 1;
            let mints = steps
                .iter()
                .filter_map(|step| step["uses"].as_str())
                .find(|uses| uses.starts_with("actions/create-github-app-token@"));
            assert!(
                mints.is_none(),
                "{role} job {} builds the release candidate and mints a repository token \
                 ({mints:?}): the candidate is not verified until the next job, so a token \
                 here is authority granted before the check that authorises it",
                job.as_str().expect("job id")
            );
        }
    }

    assert_eq!(
        preparation_jobs, 1,
        "exactly one derived job builds the release candidate, so the assertion \
         above is not passing because it reached none"
    );
}

/// Every GitHub App token step consumes its identifier from repository
/// variables and its private key from repository secrets.
///
/// The recipe-derived fixture reaches every maintained publication route. The
/// selector follows the emitted Action owner rather than a roster of templates.
/// The census keys each derivation site on its exact emitted owner identity —
/// workflow role, job id, and step id — and asserts equality against the
/// fixture-specific roster below. Those seven identities originate in five
/// token-step template sites: three instantiate once, while
/// `PUBLISH_PHASE_TAG_JOB` and `DESTINATION_TOKEN_STEPS` each fan out twice in
/// the exhaustive recipe fixture. The roster records every emitted owner
/// instead of projecting from one production owner.
#[test]
fn every_derived_github_app_token_uses_variable_id_and_secret_key() {
    const EXPECTED_DERIVATION_SITES: &[(&str, &str, &str)] = &[
        ("release", "intentional_release", "intentional_token"),
        ("publish", "intentional_close_release", "intentional_token"),
        (
            "publish",
            "intentional_upload_deliverables",
            "intentional_token",
        ),
        (
            "publish",
            "intentional_tag_before_publication",
            "intentional_token",
        ),
        (
            "publish",
            "intentional_tag_after_publication",
            "intentional_token",
        ),
        (
            "publish",
            "intentional_publish_go_application_package_homebrew_primary",
            "intentional_destination_token",
        ),
        (
            "publish",
            "intentional_publish_rust_crate_package_homebrew_primary",
            "intentional_destination_token",
        ),
    ];

    let workflows = derived_recipe_workflows("derived-workflow-app-credentials");
    let mut derivation_sites = BTreeSet::new();

    for (role, workflow) in &workflows {
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let jobs = document["jobs"]
            .as_mapping()
            .expect("derived workflow jobs");
        for (job, body) in jobs {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            for token_step in steps.iter().filter(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.starts_with("actions/create-github-app-token@"))
            }) {
                let job = job.as_str().expect("job id");
                let step = token_step["id"].as_str().expect("token step id");
                derivation_sites.insert((
                    role.as_str().to_owned(),
                    job.to_owned(),
                    step.to_owned(),
                ));
                assert_eq!(
                    token_step["with"]["app-id"].as_str(),
                    Some("${{ vars.INTENTIONAL_GITHUB_APP_ID }}"),
                    "{role} job {job} token step {step} reads its App ID from repository variables"
                );
                assert_eq!(
                    token_step["with"]["private-key"].as_str(),
                    Some("${{ secrets.INTENTIONAL_GITHUB_APP_PRIVATE_KEY }}"),
                    "{role} job {job} token step {step} reads its private key from repository secrets"
                );
            }
        }
    }

    let expected: BTreeSet<_> = EXPECTED_DERIVATION_SITES
        .iter()
        .map(|&(role, job, step)| (role.to_owned(), job.to_owned(), step.to_owned()))
        .collect();
    assert_eq!(
        derivation_sites, expected,
        "the exhaustive recipe fixture emits every GitHub App token derivation site"
    );
}

/// Put every authority-spending job, and only those jobs, in the protected environment.
///
/// The executor guide says the preparation job "runs in no environment,
/// mints no token, and checks out without persisting credentials, so nothing it
/// does can reach the repository". The mint half is held by the test above and
/// the checkout options by the test below; this one holds the environment half,
/// which was held by nothing — `environment:` could be added to
/// `RELEASE_PREPARE_JOB` and the whole suite stayed green.
///
/// Repository and Release mutation spend minted App authority. Publisher jobs
/// spend destination authority, whether it comes from a stored credential, a
/// job token, or an OpenID Connect identity. Both irreversible classes belong
/// behind the environment gate. Preparation, build, retrieval, and verification
/// jobs spend none of them and remain outside it.
///
/// Both directions of non-vacuity are checked. A run that reached no managed job,
/// or one where nothing declares an environment, would satisfy the loop while
/// proving nothing.
#[test]
fn places_only_authority_spending_jobs_in_the_protected_environment() {
    let (job_prefix, workflows) =
        prefixed_derived_recipe_workflows("derived-workflow-environments", "release-automation");
    let publisher_prefix = format!("{job_prefix}publish_");
    let mut managed = 0usize;
    let mut with_environment = 0usize;
    let mut without_environment = 0usize;

    for (role, workflow) in &workflows {
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let jobs = document["jobs"]
            .as_mapping()
            .expect("derived workflow jobs");
        for (job, body) in jobs {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            if !steps
                .iter()
                .any(|step| step["id"].as_str() == Some(OWNERSHIP_SENTINEL))
            {
                continue;
            }
            managed += 1;
            let job = job.as_str().expect("job id");
            let environment = body["environment"].as_str();
            let mints = steps.iter().any(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.starts_with("actions/create-github-app-token@"))
            });
            let publishes = job.starts_with(&publisher_prefix);
            let spends_authority = mints || publishes;
            if environment.is_some() {
                with_environment += 1;
            } else {
                without_environment += 1;
            }
            assert_eq!(
                environment.is_some(),
                spends_authority,
                "{role} job {job} environment {environment:?} disagrees with its authority-spending classification: mints={mints}, publishes={publishes}"
            );
        }
    }

    assert!(managed > 0, "the fixture derived managed jobs to check");
    assert!(
        with_environment > 0,
        "at least one managed job declares an environment, so the assertion above \
         is not passing because nothing ever declares one"
    );
    assert!(
        without_environment > 0,
        "at least one managed job remains outside the environment, so the assertion above is not passing because every job declares one"
    );
}

/// Every checkout step a derived workflow carries, paired with its job id.
///
/// The walk reads the parsed document rather than a roster of templates, so a
/// managed job added later is covered without a second list remembering it.
fn collect_checkouts(document: &Value) -> Vec<(String, Value)> {
    let mut checkouts = Vec::new();
    let Some(jobs) = document["jobs"].as_mapping() else {
        return checkouts;
    };
    for (job, body) in jobs {
        let Some(steps) = body["steps"].as_sequence() else {
            continue;
        };
        for step in steps {
            if step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/checkout@"))
            {
                checkouts.push((job.as_str().expect("job id").to_owned(), step.clone()));
            }
        }
    }
    checkouts
}

/// Count authored checkout steps independently of the YAML tree walk.
fn textual_checkout_count(workflow: &str) -> usize {
    workflow
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            (line.starts_with("uses:") || line.starts_with("- uses:"))
                && line.contains("actions/checkout@")
        })
        .count()
}

/// Checkout option whose absence removes a guarantee the release protocol needs.
#[derive(Clone, Copy)]
enum CheckoutOption {
    FetchDepth,
    FetchTags,
    PersistCredentials,
}

impl CheckoutOption {
    const ALL: [Self; 3] = [Self::FetchDepth, Self::FetchTags, Self::PersistCredentials];

    const fn key(self) -> &'static str {
        match self {
            Self::FetchDepth => "fetch-depth",
            Self::FetchTags => "fetch-tags",
            Self::PersistCredentials => "persist-credentials",
        }
    }

    fn required(self) -> Value {
        match self {
            Self::FetchDepth => Value::from(0),
            Self::FetchTags => Value::from(true),
            Self::PersistCredentials => Value::from(false),
        }
    }

    /// What a release run loses when the option is absent or wrong.
    const fn guarantee(self) -> &'static str {
        match self {
            Self::FetchDepth => {
                "verification rebuilds the candidate from S, so a shallow checkout cannot reach it"
            }
            Self::FetchTags => {
                "version authority and the global release tag are derived from annotated tags, which a checkout without them cannot see"
            }
            Self::PersistCredentials => {
                "an unprivileged job that persists the runner token holds authority the protocol grants only to the privileged job"
            }
        }
    }
}

/// Hold the module header's claim about managed checkouts to the derived text.
///
/// `templates.rs` states that every managed checkout requests tags explicitly
/// rather than inheriting them, and explains why shortening a fetch would
/// silently remove a guarantee. That explanation is the only place the property
/// existed: the options are spelled in eleven separate template literals, and a
/// twelfth added without them would have failed nothing.
///
/// The tree walk is cross-checked against an independent textual count so the
/// assertions cannot pass by reaching no checkout at all. A selector that
/// matched nothing would otherwise satisfy every `for` body below.
#[test]
fn every_managed_checkout_states_the_options_the_release_protocol_depends_on() {
    let workflows = derived_recipe_workflows("derived-workflow-checkouts");
    let mut total = 0usize;

    for (role, workflow) in &workflows {
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let checkouts = collect_checkouts(&document);
        assert_eq!(
            checkouts.len(),
            textual_checkout_count(workflow),
            "the {role} tree walk reads every checkout the derived text carries"
        );
        assert!(
            !checkouts.is_empty(),
            "the {role} workflow derives at least one checkout to check"
        );
        total += checkouts.len();

        for (job, step) in checkouts {
            let with = step["with"]
                .as_mapping()
                .unwrap_or_else(|| panic!("{role} job {job} checkout states its options"));
            for option in CheckoutOption::ALL {
                let stated = with.get(Value::from(option.key())).unwrap_or_else(|| {
                    panic!(
                        "{role} job {job} checkout omits {}: {}",
                        option.key(),
                        option.guarantee()
                    )
                });
                assert_eq!(
                    stated,
                    &option.required(),
                    "{role} job {job} checkout sets {} to {stated:?}: {}",
                    option.key(),
                    option.guarantee()
                );
            }
        }
    }

    assert!(
        total >= workflows.len(),
        "every derived workflow contributed at least one checkout: {total} across {} workflows",
        workflows.len()
    );
}

/// Recipe identity carried by a derived publisher's observer Action.
fn recipe_identity(step: &Value) -> Option<(String, String, String)> {
    step["uses"]
        .as_str()
        .is_some_and(|action| action.contains("/verify-publication@"))
        .then_some(())?;
    let inputs = step["with"].as_mapping()?;
    let target = inputs[Value::from("target")].as_str()?;
    Some((
        inputs[Value::from("release-unit")].as_str()?.to_owned(),
        inputs[Value::from("publisher")].as_str()?.to_owned(),
        if target.is_empty() { "primary" } else { target }.to_owned(),
    ))
}

/// Parse every workflow and every shell body the workflow derivation emits.
#[test]
fn derived_workflows_and_their_shell_bodies_parse() {
    let workflows = derived_recipe_workflows("derived-workflow-syntax");
    let directory = tempfile::tempdir().expect("temporary workflow directory");
    let mut managed_jobs = Vec::new();
    let mut shell_bodies = Vec::new();
    let mut reached_recipes = BTreeSet::new();

    for (role, workflow) in &workflows {
        let path = directory.path().join(format!("{role}.yml"));
        std::fs::write(&path, workflow).expect("write derived workflow");
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let before = shell_bodies.len();
        collect_run_bodies(&document, &mut shell_bodies);
        assert_eq!(
            shell_bodies.len() - before,
            textual_run_count(workflow),
            "the {role} tree walk reads every run key the derived text carries"
        );

        for (job, body) in document["jobs"]
            .as_mapping()
            .expect("derived workflow jobs")
        {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            if steps
                .iter()
                .any(|step| step["id"].as_str() == Some(OWNERSHIP_SENTINEL))
            {
                let run_count = steps.iter().filter(|step| step["run"].is_string()).count();
                managed_jobs.push((*role, job.as_str().expect("job id").to_owned(), run_count));
                reached_recipes.extend(steps.iter().filter_map(recipe_identity));
            }
        }

        let output = actionlint(&path).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            output.status.success(),
            "actionlint rejected the derived {role} workflow:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let expected_recipes = derived_recipes()
        .iter()
        .map(|recipe| {
            (
                recipe.capability.as_str().to_owned(),
                recipe.publisher.as_str().to_owned(),
                recipe.target.to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        reached_recipes, expected_recipes,
        "derived managed workflow reaches every maintained recipe"
    );
    assert!(
        managed_jobs.iter().any(|(role, job, bodies)| {
            *role == WorkflowRole::Publish && job.contains("publish_") && *bodies > 0
        }),
        "the publish workflow derives publisher shell: {managed_jobs:?}"
    );
    assert!(
        managed_jobs.iter().any(|(role, job, bodies)| {
            *role == WorkflowRole::Publish && job.ends_with("close_release") && *bodies > 0
        }),
        "the publish workflow derives closure shell: {managed_jobs:?}"
    );
    assert!(
        shell_bodies.iter().all(|body| !body.contains("${{")),
        "derived run bodies route runner expressions through env before shell parsing"
    );

    for (index, body) in shell_bodies.iter().enumerate() {
        let output = shellcheck(body).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            output.status.success(),
            "shellcheck rejected derived run body {index}:\n{}{}\n{body}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
