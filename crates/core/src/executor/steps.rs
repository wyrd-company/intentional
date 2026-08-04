// ---
// relationships:
//   implements: github-release-executor
// ---

//! Repository-local recipe steps one publisher job runs.
//!
//! A publisher job's credential-bearing work is the recipe's, not the portable
//! command's: Intentional never holds a registry credential and never speaks a
//! registry protocol, so authentication, promotion, destination readback and
//! consumer retrieval all happen in `run:` bodies this module derives. What
//! reaches the portable command is one schema-backed publication observation.
//!
//! Four rules shape every script here.
//!
//! Values arrive through `env:` and are read as shell variables. A `${{ }}`
//! expansion inside a `run:` body is textual substitution into shell source, and
//! so is a configured value interpolated into a template. These bodies hold
//! publisher credentials, so a configured name reaches a command as a quoted
//! shell variable and is validated before it is derived at all.
//!
//! Nothing a script writes into the observation is asserted by the script where
//! the graph already proved it. The subject's version and digest are the build
//! job's outputs, which `intentional evidence built-subject` derived from the
//! verified global release tag and from the bytes the build emitted, so a
//! recipe cannot record its publication under another release's version or
//! against bytes it did not promote.
//!
//! A script observes; it does not adjudicate. A destination that disagrees, or
//! that has not indexed the release yet, is written into the observation as
//! `conflict` or `pending` and the step succeeds. `intentional verify
//! publication` is what decides whether that observation completes the
//! publication, so the decision stays in one place and stays testable.
//!
//! A probe that did not succeed is not an answer. Every check that gates a
//! bootstrap credential or an immutable submission separates "the destination
//! does not hold this" from "the check did not complete", because the two are
//! one shell exit status apart and only the first may pass. The readback loops
//! collapse them, deliberately: neither reaches a credential nor submits
//! anything, and both treat an unanswered check as a publication that is not
//! observable yet, which is what a bounded wait is for.

use crate::config::ReleaseUnitConfig;
use crate::executor::names::{self, SuppliedName};
use crate::executor::recipe::{Packager, SelectedPublication, PRIMARY_TARGET};
use crate::executor::workflow::{scalar, COSIGN_INSTALLER_ACTION, SETUP_CRANE_ACTION};
use crate::model::{AttachedComponent, PublisherKind};
use crate::publication::observation::ConsistencyPolicy;

/// Everything one publication's recipe steps are derived from.
pub(super) struct RecipeContext<'a> {
    /// Publication whose destination the steps reach.
    pub publication: &'a SelectedPublication,
    /// Release unit's configuration, carrying any secret-name override.
    pub unit: &'a ReleaseUnitConfig,
    /// Identity every configured destination resolves the subject by.
    pub subject_identity: &'a str,
    /// Build job whose outputs carry the sealed subject version and digest.
    pub build_job: &'a str,
    /// Release-unit-relative directory the packager runs in.
    pub working_directory: &'a str,
    /// Observation path the recipe writes and the portable command reads.
    ///
    /// Both spell it as a `runner.temp` expression rather than as the runner's
    /// `RUNNER_TEMP` variable, because a workflow `env:` value is a literal
    /// string: only a `${{ }}` expression is resolved before the shell sees it.
    pub observation: &'a str,
    /// Scratch directory the readback and retrieval work in.
    pub work: &'a str,
    /// Workspace root, read for the native configuration a probe must not inherit.
    pub root: &'a std::path::Path,
}

/// Repository-local steps divided by the authority their job requires.
pub(super) struct RecipeSteps {
    /// Steps that authenticate and publish, and normally also retrieve.
    pub publisher: String,
    /// Consumer retrieval isolated from publish authority when required.
    pub retrieval: Option<String>,
}

/// Every process variable a maintained recipe's probe inherits.
///
/// The list is the whole of what survives `env -i`, and it is one list rather
/// than a shell fragment per adapter so that adding a member is a change to a
/// named set. Each is here because the client cannot be found or run without
/// it: `PATH` locates the executable, and `HOME` and `RUSTUP_HOME` are how a
/// proxied toolchain resolves the binary it stands in for.
///
/// Membership is unconditional. A member added by testing whether it is set is
/// membership the process environment decides, which is the shape that let
/// `RUSTUP_HOME` back in after the environment had supposedly been closed.
///
/// Because these carry runner values, a repository able to set them would
/// choose the client that answers a probe whose answer gates a long-lived
/// credential. Convergence therefore refuses a workflow that declares any of
/// them, which is what makes calling them the runner's contract true rather
/// than hopeful.
pub(super) const INHERITED_ENVIRONMENT: [&str; 3] = ["PATH", "HOME", "RUSTUP_HOME"];

/// The inherited members as one shell array fragment.
fn inherited_environment() -> String {
    INHERITED_ENVIRONMENT
        .iter()
        .map(|name| format!("{name}=\"${{{name}:-}}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `find` predicate selecting one packager's GitHub-hosted deliverables.
///
/// A GitHub-hosted deliverable is a file the build produced whose consumer
/// resolves it from the release's own GitHub Release rather than from a
/// registry. Only GoReleaser produces any today: an npm tarball, a `.crate`, an
/// OCI layout and a Dev Container Feature all reach a registry, so a build job
/// for those packagers hands the upload job nothing.
///
/// GoReleaser writes what it distributes at the top of its distribution tree
/// and everything else below it, so depth is the rule rather than a list of
/// extensions the packager could add to. Descriptors are excluded structurally:
/// a Homebrew formula lives under `homebrew/` and an Arch package's sources
/// under `aur/`, and their publisher jobs promote them into repositories. What
/// remains at the top is the archives, the checksum file, and the native
/// packages, minus the three documents the packager writes to describe its own
/// run. Those three are named because they are build metadata rather than
/// anything a consumer resolves; a release that published them would inventory
/// GoReleaser's internal state as a deliverable.
pub(super) const fn github_hosted_deliverables(packager: Packager) -> Option<&'static str> {
    match packager {
        Packager::GoReleaser => {
            Some("! -name artifacts.json ! -name metadata.json ! -name config.yaml")
        }
        Packager::Npm | Packager::Cargo | Packager::Buildx | Packager::DevContainerCli => None,
    }
}

/// `find` predicate selecting the deliverables one publication consumes.
///
/// A draft-dependent publication's handoff inventories the assets that
/// publication retrieves, not every asset the subject produced. The split is
/// the packager's own: `nfpms` writes one file per declared format and each
/// system-package adapter distributes exactly its own format, while a Homebrew
/// formula and an Arch `PKGBUILD` resolve the release archives their descriptor
/// points at. Handing an adapter the other half would make it download and
/// digest bytes its consumer path never resolves and record that as its
/// retrieval.
///
/// The descriptor adapters therefore exclude the formats the repository
/// declared rather than the two a system-package adapter happens to
/// distribute. `nfpms` builds five formats and a release unit is free to ask
/// for any of them, so a fixed pair is an extension denylist over an open set
/// -- the same shape [`github_hosted_deliverables`] avoids on the placed side,
/// and with the same consequence when the set grows.
///
/// The declared formats never reach the derived shell. Each is mapped to an
/// extension the derivation owns, and a format with no mapping is refused, so
/// the predicate is built entirely from literals this module chose.
pub(super) fn consumed_deliverables(
    publisher: PublisherKind,
    release_unit: &str,
    nfpm_formats: &[String],
) -> Result<Option<String>, StepsRefusal> {
    let extension = crate::executor::goreleaser::nfpm_extension;
    match publisher {
        PublisherKind::Rpm | PublisherKind::Apt => {
            Ok(crate::executor::goreleaser::nfpm_format(publisher)
                .and_then(extension)
                .map(|native| format!("-name '*.{native}'")))
        }
        PublisherKind::Homebrew | PublisherKind::Aur => {
            let mut excluded = std::collections::BTreeSet::new();
            for format in nfpm_formats {
                let native = extension(format).ok_or_else(|| StepsRefusal {
                    code: "nfpm-format-underived",
                    message: format!(
                        "release unit {release_unit} declares nfpm format {format:?}, which the derivation cannot recognise as a native package; a descriptor publisher would retrieve it as one of the release archives its formula resolves"
                    ),
                    path: Some(format!("release-units.{release_unit}")),
                })?;
                excluded.insert(format!("! -name '*.{native}'"));
            }
            Ok(Some(excluded.into_iter().collect::<Vec<_>>().join(" ")))
        }
        PublisherKind::Npm | PublisherKind::Cargo | PublisherKind::Oci => Ok(None),
    }
}

/// Conventional GitHub secret holding npm's bootstrap token.
const NPM_TOKEN_SECRET: &str = "NPM_TOKEN";
/// Conventional GitHub secret holding Cargo's bootstrap registry token.
const CARGO_TOKEN_SECRET: &str = "CARGO_REGISTRY_TOKEN";

/// npm registry serving the adapter's primary destination.
const NPMJS_REGISTRY: &str = "https://registry.npmjs.org";
/// npm registry serving GitHub Package Registry.
const GITHUB_PACKAGES_REGISTRY: &str = "https://npm.pkg.github.com";
/// Destination identity GitHub Package Registry publications record.
const GITHUB_PACKAGES_DESTINATION: &str = "npm.pkg.github.com";

/// Default Cargo registry, which is also the one that implements trusted publishing.
const CRATES_IO: &str = "crates.io";

/// Lowest npm release that implements registry trusted publishing.
///
/// A stock runner's bundled npm is older than this on the images this executor
/// targets, and an older client falls back to looking for a token that a
/// trusted-publishing repository deliberately does not hold. Stating the
/// requirement as a range makes the failure a resolution error naming the
/// version rather than an authentication error naming nothing.
const NPM_TRUSTED_PUBLISHING_RANGE: &str = ">=11.5.1";

/// Why one publication derives no maintained recipe steps.
///
/// The code travels with the message because these are different answers to the
/// operator -- a recipe the packager's adapter could not derive, a destination
/// its client resolves for itself, a native package format the derivation does
/// not recognise -- and collapsing them into one code would make each
/// unreportable at the boundary that reads codes.
pub(super) struct StepsRefusal {
    /// Diagnostic code the workflow comparison reports this refusal under.
    pub code: &'static str,
    /// Whole message, already naming the publication it refuses.
    pub message: String,
    /// Configuration key the refusal is about, when one key wrote it.
    ///
    /// A refusal about the recipe as a whole names the release unit, which is
    /// what the caller falls back to. A refusal about a value an author typed
    /// names the key they typed it into, so the diagnostic points at the line
    /// to edit rather than at the unit that contains it.
    pub path: Option<String>,
}

impl StepsRefusal {
    /// A recipe this packager's own adapter could not derive steps for.
    fn underivable(identity: &str, message: &str) -> Self {
        Self {
            code: "recipe-underivable",
            message: format!(
                "publication {identity} derives no maintained recipe steps: {message}"
            ),
            path: None,
        }
    }

    /// A destination this packager's client resolves for itself.
    fn not_overridable(message: String, path: String) -> Self {
        Self {
            code: "destination-not-overridable",
            message,
            path: Some(path),
        }
    }
}

/// Steps one publication's maintained recipe contributes to its publisher job.
///
/// The three packagers below each keep one arm rather than sharing a collapsed
/// one. Their recipes are owned by separate tasks working from a common base,
/// and a shared arm makes one edit each of them has to make into one edit they
/// have to make together.
pub(super) fn recipe_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, StepsRefusal> {
    steps_for(context).map(|steps| RecipeSteps {
        publisher: steps
            .publisher
            .replace("@INHERITED@", &inherited_environment()),
        retrieval: steps
            .retrieval
            .map(|retrieval| retrieval.replace("@INHERITED@", &inherited_environment())),
    })
}

/// Whether workflow derivation has a complete publisher recipe for this pair.
pub(super) const fn recipe_is_derived(packager: Packager, publisher: PublisherKind) -> bool {
    !matches!(
        (packager, publisher),
        (
            Packager::GoReleaser,
            PublisherKind::Rpm | PublisherKind::Apt
        )
    )
}

/// One publication's recipe steps before the shared placeholders are rendered.
fn steps_for(context: &RecipeContext<'_>) -> Result<RecipeSteps, StepsRefusal> {
    let identity = context.publication.identity();
    let underivable = |message: String| StepsRefusal::underivable(&identity, &message);
    match context.publication.packager {
        Packager::Npm => npm_steps(context).map_err(underivable),
        Packager::Cargo => cargo_steps(context)
            .map(RecipeSteps::together)
            .map_err(underivable),
        Packager::GoReleaser => goreleaser_steps(context).map(RecipeSteps::together),
        Packager::Buildx | Packager::DevContainerCli => {
            oci_steps(context).map(RecipeSteps::together)
        }
    }
}

impl RecipeSteps {
    /// Keep a recipe in one job when publication and retrieval share authority.
    fn together(publisher: String) -> Self {
        Self {
            publisher,
            retrieval: None,
        }
    }
}

/// Credential and promotion steps one GoReleaser destination requires.
///
/// GoReleaser's open-source distribution has no command that publishes a `dist/`
/// tree a previous invocation built, so a maintained Go recipe cannot reach its
/// destination by running the packager again: a second `goreleaser release`
/// rebuilds from source and produces a subject whose digest cannot equal the one
/// the release sealed. These steps therefore promote the file the build already
/// produced, as an ordinary repository-local operation against the destination's
/// own repository.
///
/// Each maintained Go recipe promotes into a different repository under a
/// different authority, so what varies between them is which repository receives
/// the file and which credential reaches it. Everything else — the sealed
/// subject, the release tag the commit message carries, the committer identity —
/// is the part every Go destination shares.
fn goreleaser_steps(context: &RecipeContext<'_>) -> Result<String, StepsRefusal> {
    let identity = context.publication.identity();
    // RPM and APT distribute the deliverable itself rather than a descriptor
    // that points at one, so their consumer path is the GitHub Release asset and
    // the managed upload job places it there. What these adapters still lack is
    // a maintained recipe of their own: nothing authenticates a package index,
    // reads the destination back, or retrieves the release the way a consumer
    // would. Deriving a publisher job without one would verify a publication it
    // never performed, so the refusal names the recipe rather than the upload
    // the design has since settled and this workflow now derives.
    if !recipe_is_derived(context.publication.packager, context.publication.publisher) {
        return Err(StepsRefusal {
            code: "maintained-recipe-underived",
            message: format!(
                "publication {identity} distributes a GitHub-hosted deliverable the managed upload job places on the draft Release, but no maintained {} recipe is derived to reach its package index, so the publisher job would verify a publication it never performed",
                context.publication.publisher
            ),
            path: None,
        });
    }
    let destination = context.publication.destination.clone().ok_or(StepsRefusal {
        code: "destination-underived",
        message: format!(
            "publication {identity} promotes into a destination repository, but none is configured or derivable"
        ),
        path: None,
    })?;
    let mut credential = String::new();
    let mut environment = format!("      @ENVVAR@DESTINATION: {}\n", scalar(&destination));
    environment.push_str(&format!(
        "      @ENVVAR@COMMITTER_NAME: {}\n      @ENVVAR@COMMITTER_EMAIL: {}\n",
        scalar(crate::release::build::RELEASE_IDENTITY_NAME),
        scalar(crate::release::build::RELEASE_IDENTITY_EMAIL),
    ));
    let command = match context.publication.publisher {
        PublisherKind::Homebrew => {
            let (owner, name) = destination.split_once('/').ok_or(StepsRefusal {
                code: "destination-malformed",
                message: format!(
                    "publication {identity} names tap repository {destination:?}, which is not owner/name"
                ),
                path: None,
            })?;
            credential.push_str(
                &DESTINATION_TOKEN_STEPS
                    .replace("@DESTINATION_OWNER@", &scalar(owner))
                    .replace("@DESTINATION_NAME@", &scalar(name)),
            );
            environment.push_str(
                "      GITHUB_TOKEN: ${{ steps.@JOB@destination_token.outputs.token }}\n",
            );
            HOMEBREW_PROMOTE_COMMAND
        }
        PublisherKind::Aur => {
            environment.push_str(&format!(
                "      @ENVVAR@AUR_KEY: ${{{{ secrets.@ENVVAR@AUR_KEY }}}}\n      @ENVVAR@AUR_HOST_FINGERPRINT: {}\n",
                scalar(AUR_HOST_FINGERPRINT)
            ));
            AUR_PROMOTE_COMMAND
        }
        publisher => unreachable!("{publisher} is refused above"),
    };
    Ok(format!(
        "{credential}  - name: {}\n    env:\n      @ENVVAR@SUBJECT: ${{{{ runner.temp }}}}/@JOB@subject/bytes\n      @ENVVAR@SUBJECT_IDENTITY: {}\n      @ENVVAR@GLOBAL_TAG: ${{{{ github.ref_name }}}}\n{environment}    run: |\n      set -euo pipefail\n{command}\n",
        scalar(&format!("Publish {identity}")),
        scalar(context.subject_identity),
    ))
}

/// Published ED25519 host key fingerprint of the Arch User Repository.
///
/// Pinned rather than accepted on first use. The recipe scopes its SSH authority
/// to one destination, and trusting whatever key answers on a runner would hand
/// that authority to an unauthenticated peer. A host whose key does not match
/// this pin fails the job, so a stale pin is a loud failure rather than a silent
/// downgrade.
const AUR_HOST_FINGERPRINT: &str = "SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4";

/// Steps minting a short-lived token for one destination repository.
///
/// The App is installed narrowly on the tap or index repository, and the token
/// names that repository explicitly, so a job that promotes into one destination
/// cannot write to another repository the App happens to be installed on.
const DESTINATION_TOKEN_STEPS: &str = r#"  - id: @JOB@destination_token
    name: Mint a short-lived token for the destination repository
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
      owner: @DESTINATION_OWNER@
      repositories: @DESTINATION_NAME@
"#;

/// Promote the generated Homebrew formulas into the configured tap repository.
///
/// The packager writes each formula to `homebrew/<directory>/<name>.rb` inside
/// the distribution tree, where the directory and the name are the ones the
/// release unit's own `brews` entry declares, and publishes it to that same
/// relative path in the tap. Promotion therefore preserves the path rather than
/// choosing one: a tap whose formulas do not live under `Formula` is stating
/// where they live, and this recipe's premise is that the native configuration
/// is the authority on that.
///
/// Every generated formula is promoted. A release unit with two `brews` entries
/// publishes both, and scoping the search to the packager's `homebrew` output
/// keeps a cask or any other generated Ruby file from being promoted as one.
///
/// A rerun that finds the tap already carrying this release commits nothing and
/// still succeeds, because the destination readback that follows is what decides
/// whether the publication is present rather than whether this step wrote.
const HOMEBREW_PROMOTE_COMMAND: &str = r#"      generated="${@ENVVAR@SUBJECT}/homebrew"
      test -d "${generated}"
      mapfile -t -d '' formulas < <(find "${generated}" -type f -name '*.rb' -print0 | sort -z)
      test "${#formulas[@]}" -gt 0
      rm -rf "${RUNNER_TEMP}/@JOB@tap"
      git clone --quiet --depth 1 \
        "https://x-access-token:${GITHUB_TOKEN}@github.com/${@ENVVAR@DESTINATION}.git" \
        "${RUNNER_TEMP}/@JOB@tap"
      for formula in "${formulas[@]}"; do
        install -D -m 644 "${formula}" \
          "${RUNNER_TEMP}/@JOB@tap/${formula#"${generated}/"}"
      done
      git -C "${RUNNER_TEMP}/@JOB@tap" add --all
      if [ -n "$(git -C "${RUNNER_TEMP}/@JOB@tap" status --porcelain)" ]; then
        git -C "${RUNNER_TEMP}/@JOB@tap" \
          -c user.name="${@ENVVAR@COMMITTER_NAME}" \
          -c user.email="${@ENVVAR@COMMITTER_EMAIL}" \
          commit --quiet -m "${@ENVVAR@SUBJECT_IDENTITY} ${@ENVVAR@GLOBAL_TAG}"
        git -C "${RUNNER_TEMP}/@JOB@tap" push --quiet origin HEAD
      fi"#;

/// Promote the generated Arch package sources into the Arch User Repository.
///
/// The Arch User Repository is a Git host of its own rather than a GitHub
/// repository, so its authority is an SSH key under the conventional secret
/// name the recipe states. The key never reaches Intentional configuration; the
/// recipe names the secret and the repository supplies it.
///
/// The packager writes this package's sources as `aur/<package>.pkgbuild` and
/// `aur/<package>.srcinfo`, and the names `PKGBUILD` and `.SRCINFO` are the ones
/// the Arch User Repository requires at the destination. Both sides are named
/// exactly: reading the package's own two files keeps a release unit with more
/// than one `aur` entry from sweeping its sibling's sources into this package,
/// and installing them under their required names is what makes the result a
/// package `makepkg` can read at all.
///
/// The host key is pinned rather than accepted from whatever answers on the
/// runner. The recipe is careful to scope this SSH authority to one destination,
/// and trusting the host on first use would hand that authority to an
/// unauthenticated peer. A host whose key does not match the pin fails the job.
///
/// A package the Arch User Repository does not yet carry cannot be cloned, so an
/// absent package is initialized locally and created by the initial push, which
/// is how the Arch User Repository registers one.
const AUR_PROMOTE_COMMAND: &str = r#"      pkgbuild="${@ENVVAR@SUBJECT}/aur/${@ENVVAR@DESTINATION}.pkgbuild"
      srcinfo="${@ENVVAR@SUBJECT}/aur/${@ENVVAR@DESTINATION}.srcinfo"
      test -f "${pkgbuild}"
      test -f "${srcinfo}"
      install -d -m 700 "${HOME}/.ssh"
      printf '%s\n' "${@ENVVAR@AUR_KEY}" > "${HOME}/.ssh/@JOB@aur"
      chmod 600 "${HOME}/.ssh/@JOB@aur"
      ssh-keyscan -t ed25519 aur.archlinux.org > "${RUNNER_TEMP}/@JOB@aur-host-key"
      test "$(ssh-keygen -l -f "${RUNNER_TEMP}/@JOB@aur-host-key" | cut -d' ' -f2)" \
        = "${@ENVVAR@AUR_HOST_FINGERPRINT}"
      cat "${RUNNER_TEMP}/@JOB@aur-host-key" >> "${HOME}/.ssh/known_hosts"
      export GIT_SSH_COMMAND="ssh -i ${HOME}/.ssh/@JOB@aur -o IdentitiesOnly=yes"
      rm -rf "${RUNNER_TEMP}/@JOB@aur"
      if ! git clone --quiet "ssh://aur@aur.archlinux.org/${@ENVVAR@DESTINATION}.git" \
        "${RUNNER_TEMP}/@JOB@aur"; then
        git init --quiet --initial-branch=master "${RUNNER_TEMP}/@JOB@aur"
        git -C "${RUNNER_TEMP}/@JOB@aur" remote add origin \
          "ssh://aur@aur.archlinux.org/${@ENVVAR@DESTINATION}.git"
      fi
      install -m 644 "${pkgbuild}" "${RUNNER_TEMP}/@JOB@aur/PKGBUILD"
      install -m 644 "${srcinfo}" "${RUNNER_TEMP}/@JOB@aur/.SRCINFO"
      git -C "${RUNNER_TEMP}/@JOB@aur" add --all
      if [ -n "$(git -C "${RUNNER_TEMP}/@JOB@aur" status --porcelain)" ]; then
        git -C "${RUNNER_TEMP}/@JOB@aur" \
          -c user.name="${@ENVVAR@COMMITTER_NAME}" \
          -c user.email="${@ENVVAR@COMMITTER_EMAIL}" \
          commit --quiet -m "${@ENVVAR@SUBJECT_IDENTITY} ${@ENVVAR@GLOBAL_TAG}"
        git -C "${RUNNER_TEMP}/@JOB@aur" push --quiet origin HEAD:master
      fi"#;

/// Index one alternate Cargo registry is declared with, if the workspace declares one.
///
/// Read at derivation rather than inherited by the probe: the probe copying the
/// whole file made every key in it an input to the decision that unlocks a
/// bootstrap credential.
fn cargo_registry_index(root: &std::path::Path, registry: &str) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".cargo/config.toml")).ok()?;
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    document
        .get("registries")?
        .get(registry)?
        .get("index")?
        .as_str()
        .map(str::to_owned)
}

/// Cargo's environment spelling of one configured registry name.
fn environment_fragment(registry: &str) -> String {
    registry
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Bounded eventual-consistency values one adapter's policy states, in seconds.
///
/// The recipe waits for its own destination to become observable, so the bound
/// it waits under has to be the same one `intentional verify publication`
/// applies to what it reports. Rendering it from the maintained policy is what
/// keeps the two from drifting into a recipe that gives up before the command
/// would, or one that outlives the job.
fn policy_environment(publisher: PublisherKind) -> String {
    let policy = ConsistencyPolicy::maintained(publisher);
    format!(
        "      @ENVVAR@INTERVAL: {}\n      @ENVVAR@BACKOFF: {}\n      @ENVVAR@MAXIMUM_INTERVAL: {}\n      @ENVVAR@DEADLINE: {}\n",
        scalar(&policy.interval.as_secs().to_string()),
        scalar(&policy.backoff.to_string()),
        scalar(&policy.maximum_interval.as_secs().to_string()),
        scalar(&policy.deadline.as_secs().to_string()),
    )
}

/// Shell that resolves the release the graph proved, for a step's `env:` block.
///
/// Every one of these is a value some earlier job established: the subject's
/// identity is the derivation's, and its version and digest are the ones the
/// build job's `intentional evidence built-subject` produced.
fn subject_environment(context: &RecipeContext<'_>) -> String {
    format!(
        "      @ENVVAR@SUBJECT: ${{{{ runner.temp }}}}/@JOB@subject/bytes\n      @ENVVAR@SUBJECT_IDENTITY: {}\n      @ENVVAR@VERSION: ${{{{ needs.{}.outputs.version }}}}\n      @ENVVAR@SUBJECT_DIGEST: ${{{{ needs.{}.outputs.digest }}}}\n",
        scalar(context.subject_identity),
        context.build_job,
        context.build_job,
    )
}

/// Observation members every recipe writes identically, for a step's `env:`.
///
/// The retrieval mode among them is the one the selected recipe fixes, not a
/// spelling the script chose: `intentional verify publication` refuses any
/// other, and a recipe that named its own would be discovered by that refusal
/// on a release runner rather than by derivation here.
/// The retrieval client is routed separately from the packager because the two
/// are the same program for a language registry and are not for an OCI
/// destination: a Buildx subject is retrieved by the registry client, not by
/// the builder that produced it. Naming one and printing it twice would record
/// a retrieval that did not happen the way it says it did.
fn observation_environment(
    context: &RecipeContext<'_>,
    kind: &str,
    packager: &str,
    client: &str,
) -> String {
    format!(
        "      @ENVVAR@OBSERVATION: {}\n      @ENVVAR@RELEASE_UNIT: {}\n      @ENVVAR@PUBLISHER: {}\n      @ENVVAR@TARGET: {}\n      @ENVVAR@WORK: {}\n      @ENVVAR@SUBJECT_KIND: {}\n      @ENVVAR@PACKAGER_ID: {}\n      @ENVVAR@RETRIEVAL_MODE: {}\n      @ENVVAR@RETRIEVAL_CLIENT: {}\n",
        scalar(context.observation),
        scalar(&context.publication.release_unit),
        scalar(context.publication.publisher.as_str()),
        scalar(&context.publication.target),
        scalar(context.work),
        scalar(kind),
        scalar(packager),
        scalar(context.publication.retrieval.as_str()),
        scalar(client),
    )
}

/// Shell writing any one of the three observation documents a recipe produces.
///
/// Every literal the schema fixes appears once. Three copies of a schema
/// identity is three chances for one of them to be edited alone, and a document
/// that names the wrong schema or misspells a member is rejected by the loader
/// three jobs downstream, as a verification failure naming the destination
/// rather than the recipe. The helpers are also the reason those documents can
/// be executed by a test at all: everything the adapter computes reaches them
/// as a variable, so the writing can be driven without reaching a registry.
const OBSERVE: &str = r#"      @ENVVAR@observe_header() {
        # $schema is a literal YAML key, not a shell expansion.
        # shellcheck disable=SC2016
        printf '$schema: https://intentional.foo/schemas/publication-observation/v1\n'
        printf 'contract: publication-observation-1\n'
        printf 'release-unit: "%s"\n' "${@ENVVAR@RELEASE_UNIT}"
        printf 'publisher: "%s"\n' "${@ENVVAR@PUBLISHER}"
        printf 'target: "%s"\n' "${@ENVVAR@TARGET}"
      }
      @ENVVAR@observe_state() {
        mkdir -p "$(dirname "${@ENVVAR@OBSERVATION}")"
        {
          @ENVVAR@observe_header
          printf 'state: %s\n' "$1"
          if [ "$1" = conflict ]; then printf 'conflict: "%s"\n' "$2"; fi
        } > "${@ENVVAR@OBSERVATION}"
      }
      @ENVVAR@observe_present() {
        mkdir -p "$(dirname "${@ENVVAR@OBSERVATION}")"
        {
          @ENVVAR@observe_header
          printf 'state: present\n'
          printf 'subject:\n'
          printf '  kind: "%s"\n' "${@ENVVAR@SUBJECT_KIND}"
          printf '  identity: "%s"\n' "${@ENVVAR@SUBJECT_IDENTITY}"
          printf '  version: "%s"\n' "${@ENVVAR@VERSION}"
          printf '  digest: "%s"\n' "${@ENVVAR@SUBJECT_DIGEST}"
          printf 'packager:\n'
          printf '  id: "%s"\n' "${@ENVVAR@PACKAGER_ID}"
          printf '  version: "%s"\n' "${@ENVVAR@PACKAGER_VERSION}"
          printf 'destination:\n'
          printf '  identity: "%s"\n' "${@ENVVAR@DESTINATION}"
          printf '  version: "%s"\n' "${@ENVVAR@VERSION}"
          printf '  digest: "%s"\n' "${@ENVVAR@DESTINATION_DIGEST}"
          printf 'retrieval:\n'
          printf '  mode: %s\n' "${@ENVVAR@RETRIEVAL_MODE}"
          printf '  client: "%s"\n' "${@ENVVAR@RETRIEVAL_CLIENT}"
          printf '  version: "%s"\n' "${@ENVVAR@RETRIEVAL_VERSION}"
          printf '  digest: "%s"\n' "${@ENVVAR@RETRIEVED_DIGEST}"
        } > "${@ENVVAR@OBSERVATION}"
      }
"#;

/// npm recipe: trusted publishing, promotion of the built tarball, and readback.
fn npm_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, String> {
    let primary = context.publication.target == PRIMARY_TARGET;
    let identity = context.publication.identity();
    let registry = if primary {
        NPMJS_REGISTRY
    } else {
        GITHUB_PACKAGES_REGISTRY
    };
    // Selection resolves the primary's destination identity, and verification
    // compares the observed one against it, so the recipe reports the identity
    // configuration settled rather than a second spelling of the same registry.
    let destination = context
        .publication
        .destination
        .as_deref()
        .unwrap_or(GITHUB_PACKAGES_DESTINATION);
    // GitHub Package Registry resolves a package under the owning account's
    // scope and rejects a package name that carries none. The name is the one
    // the npmjs primary publishes, so a repository that adds this destination to
    // an unscoped package has configured something the registry will refuse.
    // Saying so here names the cause; letting it derive turns it into a publish
    // failure in a job holding a token, after the primary has already shipped.
    if !primary && !context.subject_identity.starts_with('@') {
        return Err(format!(
            "GitHub Package Registry resolves {} under the publishing account's scope, and package name {:?} carries none; scope the package as @owner/name or remove the github additional target",
            context.publication.release_unit, context.subject_identity
        ));
    }
    let origin = format!(
        "release unit {} npm token-secret",
        context.publication.release_unit
    );
    let bootstrap = names::secret(
        context
            .unit
            .npm
            .as_ref()
            .and_then(|npm| npm.token_secret.as_deref())
            .map(|value| SuppliedName {
                origin: &origin,
                value,
            })
            .as_ref(),
        NPM_TOKEN_SECRET,
    )?;
    // The scope is taken from the identity the boundary already validated, so
    // the probe states which registry serves it rather than letting a project
    // configuration answer that question.
    let scope = context
        .subject_identity
        .split_once('/')
        .map_or("", |(scope, _)| scope);
    let scope_environment = format!("      @ENVVAR@SCOPE: {}\n", scalar(scope));
    let mut steps = String::new();

    // Trusted publishing is an npm client capability rather than a registry
    // negotiation, so the client that performs it has to be new enough to know
    // the exchange exists. GitHub Package Registry does not implement it, and
    // its recipe stays on the runner's own client.
    if primary {
        steps.push_str(&format!(
            "  - name: Prepare the npm client for trusted publishing\n    env:\n      @ENVVAR@NPM_RANGE: {}\n    run: |\n{}      npm install --global \"npm@${{@ENVVAR@NPM_RANGE}}\"\n      npm --version\n",
            scalar(NPM_TRUSTED_PUBLISHING_RANGE),
            STRICT_MODE,
        ));
    }

    steps.push_str(&format!(
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n      @ENVVAR@SUBJECT_IDENTITY: {}\n{scope_environment}{}    run: |\n{}{}{}",
        scalar(&format!("Authenticate the {identity} publication")),
        scalar(registry),
        scalar(context.subject_identity),
        if primary {
            format!("      @ENVVAR@BOOTSTRAP_TOKEN: ${{{{ secrets.{bootstrap} }}}}\n")
        } else {
            "      @ENVVAR@GITHUB_PACKAGES_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n".to_owned()
        },
        STRICT_MODE,
        if primary {
            const_probe()
        } else {
            String::new()
        },
        if primary {
            NPM_TRUSTED_AUTHENTICATION
        } else {
            NPM_GITHUB_AUTHENTICATION
        },
    ));

    steps.push_str(&format!(
        "  - name: {}\n    working-directory: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n{scope_environment}{}    run: |\n{}{}{}",
        scalar(&format!("Publish {identity}")),
        scalar(context.working_directory),
        scalar(registry),
        subject_environment(context),
        STRICT_MODE,
        const_probe(),
        if primary {
            NPM_PUBLISH_PRIMARY
        } else {
            NPM_PUBLISH_GITHUB
        },
    ));

    let readback = format!(
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n      @ENVVAR@DESTINATION: {}\n{scope_environment}{}{}{}{}    run: |\n{}{}{}{}{}",
        scalar(&format!("Read {identity} back and retrieve it")),
        scalar(registry),
        scalar(destination),
        if primary {
            String::new()
        } else {
            "      @ENVVAR@GITHUB_PACKAGES_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n".to_owned()
        },
        subject_environment(context),
        observation_environment(context, "npm-package", "npm", "npm"),
        policy_environment(context.publication.publisher),
        STRICT_MODE,
        if primary {
            ""
        } else {
            NPM_GITHUB_AUTHENTICATION
        },
        OBSERVE,
        const_probe(),
        npm_readback(if primary {
            NPM_RETRIEVE_PUBLIC
        } else {
            NPM_RETRIEVE_AUTHENTICATED
        }),
    );
    if primary {
        steps.push_str(&readback);
        Ok(RecipeSteps::together(steps))
    } else {
        Ok(RecipeSteps {
            publisher: steps,
            retrieval: Some(readback),
        })
    }
}

/// Shell every recipe step opens with.
///
/// The strict-mode line comes before any helper definition so a reader can see
/// at a glance that everything below it is guarded, and so a helper added later
/// cannot quietly land above the line that makes a failure fatal.
const STRICT_MODE: &str = "      set -euo pipefail\n";

/// Shell separating "the registry does not hold this" from "the check failed".
///
/// A missing package, a rate limit, a proxy failure and a 5xx are one exit
/// status in `npm view`. Collapsing them makes every transient failure look
/// like a first publication, which is the one condition that unlocks the
/// long-lived bootstrap token. The registry distinguishes them in its output,
/// so the helper does too and every caller decides on three outcomes.
///
/// The two streams are kept apart. npm writes warnings, notices and its update
/// notice to standard error on calls that succeed, so a helper that merged the
/// streams would return them as part of the answer, and the readback that uses
/// that answer as the destination digest would find it unequal to the promoted
/// integrity and write a `conflict` observation accusing the registry of
/// publishing bytes the release did not send. The classification reads the
/// error stream; the value returned is only ever what the registry answered.
///
/// The probe also inherits nothing from the repository it is releasing, for the
/// reason its Cargo counterpart does not: npm reads a project `.npmrc` from the
/// working directory, and a `@scope:registry` line there outranks `--registry`
/// for a scoped name. A repository could therefore point the existence check at
/// a registry of its choosing and have the answer decide whether a long-lived
/// credential is reached. The probe runs from a scratch directory instead, and
/// a scoped name carries its scope's registry explicitly, so what the check
/// asked is what derivation chose.
const NPM_HOLDS: &str = r#"      @ENVVAR@npm_holds() {
        mkdir -p "${RUNNER_TEMP}/@JOB@npm-probe"
        if @ENVVAR@VIEW="$(cd "${RUNNER_TEMP}/@JOB@npm-probe" \
          && env -i "${@ENVVAR@ALLOWED[@]}" \
            npm view "$1" dist.integrity --registry "${@ENVVAR@REGISTRY}" \
            "${@ENVVAR@SCOPE_ARGUMENTS[@]}" 2>"${RUNNER_TEMP}/@JOB@npm-error")"; then
          printf '%s' "${@ENVVAR@VIEW}"
          return 0
        fi
        case "$(cat "${RUNNER_TEMP}/@JOB@npm-error")" in
          *E404*|*"404 Not Found"*) return 1 ;;
          *) cat "${RUNNER_TEMP}/@JOB@npm-error" >&2 ; return 2 ;;
        esac
      }
"#;

/// The probe helper together with the scope arguments it reads.
fn const_probe() -> String {
    format!("{NPM_ALLOWED}{NPM_SCOPE_ARGUMENTS}{NPM_HOLDS}")
}

/// The environment the npm probe is given, in place of the one it would inherit.
///
/// npm reads `npm_config_*` from the process environment and a project `.npmrc`
/// from the working directory, and a repository controls both: the first
/// through its own workflow's top-level `env:`, which convergence preserves on
/// purpose, and the second through a file in the checkout. The probe decides
/// whether a first publication reaches a long-lived token, so both are cleared
/// — the working directory by running elsewhere, the environment by naming what
/// may be in it.
///
/// The members are [`INHERITED_ENVIRONMENT`], the same list the Cargo probe
/// renders, and `HOME` matters here for a second reason: the user configuration
/// the authenticate step wrote is the credential this destination legitimately
/// presents. A repository cannot choose any of those values, because
/// convergence refuses a workflow that declares them.
const NPM_ALLOWED: &str = r#"      @ENVVAR@ALLOWED=(@INHERITED@)
"#;

/// The scope registry a scoped name's probe states for itself.
///
/// npm resolves a scoped package through `@scope:registry` when one is
/// configured, so naming it on the command line is what makes `--registry` mean
/// what it says. The scope is derived from the validated package name, and it
/// reaches the command as its own argument rather than as spliced text.
const NPM_SCOPE_ARGUMENTS: &str = r#"      @ENVVAR@SCOPE_ARGUMENTS=()
      if [ -n "${@ENVVAR@SCOPE:-}" ]; then
        @ENVVAR@SCOPE_ARGUMENTS=("--${@ENVVAR@SCOPE}:registry=${@ENVVAR@REGISTRY}")
      fi
"#;

/// Trusted-publishing authentication with a protected first-publication path.
///
/// npmjs can only bind a trusted publisher to a package that already exists, so
/// the very first publication of a package has nothing to be trusted against.
/// The bootstrap token covers exactly that case and is reachable only on a
/// probe that proved the package absent: an inconclusive probe fails the job
/// rather than reaching for the token, because a rerun after a transient
/// registry failure would otherwise publish a long-established package with a
/// long-lived credential and never mention it.
const NPM_TRUSTED_AUTHENTICATION: &str = r#"      npm config set registry "${@ENVVAR@REGISTRY}"
      @ENVVAR@npm_holds "${@ENVVAR@SUBJECT_IDENTITY}" >/dev/null && @ENVVAR@HELD=0 || @ENVVAR@HELD=$?
      case "${@ENVVAR@HELD}" in
        0)
          echo "${@ENVVAR@SUBJECT_IDENTITY} exists; this publication uses its configured trusted publisher"
          exit 0
          ;;
        1) ;;
        *)
          echo "the registry did not answer whether it holds ${@ENVVAR@SUBJECT_IDENTITY}" >&2
          echo "a bootstrap token is reachable only on a proven first publication" >&2
          exit 1
          ;;
      esac
      if [ -z "${@ENVVAR@BOOTSTRAP_TOKEN:-}" ]; then
        echo "${@ENVVAR@SUBJECT_IDENTITY} does not exist yet and no bootstrap token secret is available" >&2
        echo "a trusted publisher can only be configured for an existing package" >&2
        exit 1
      fi
      @ENVVAR@HOST="${@ENVVAR@REGISTRY#https://}"
      npm config set "//${@ENVVAR@HOST%/}/:_authToken=${@ENVVAR@BOOTSTRAP_TOKEN}"
"#;

/// GitHub Package Registry authentication.
///
/// The registry implements no trusted publishing, and the workflow token it
/// does accept is scoped to this repository and expires with the job, so there
/// is no bootstrap path to expose and no long-lived secret to hold.
const NPM_GITHUB_AUTHENTICATION: &str = r#"      @ENVVAR@HOST="${@ENVVAR@REGISTRY#https://}"
      npm config set "//${@ENVVAR@HOST%/}/:_authToken=${@ENVVAR@GITHUB_PACKAGES_TOKEN}"
"#;

/// Promotion of the tarball the build job produced.
///
/// `npm publish` given a tarball path uploads exactly those bytes, so the
/// destination receives the subject the release sealed rather than a second
/// packaging of the same source. Reading the destination first is what makes a
/// rerun recover: a release already accepted is left alone and the readback
/// decides whether what is there is this release. A probe that did not answer
/// stops the job, because submitting an immutable version on a guess is the one
/// thing a rerun cannot undo.
const NPM_PUBLISH_PRIMARY: &str = r#"      @ENVVAR@TARBALL="$(find "${@ENVVAR@SUBJECT}" -maxdepth 1 -name '*.tgz' -print -quit)"
      test -n "${@ENVVAR@TARBALL}"
      @ENVVAR@npm_holds "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" >/dev/null \
        && @ENVVAR@HELD=0 || @ENVVAR@HELD=$?
      case "${@ENVVAR@HELD}" in
        0)
          echo "the destination already holds this version; the readback decides whether it is this release"
          exit 0
          ;;
        1) ;;
        *)
          echo "the registry did not answer whether it holds this version; refusing to submit" >&2
          exit 1
          ;;
      esac
      npm publish "${@ENVVAR@TARBALL}" --provenance --access public \
        --registry "${@ENVVAR@REGISTRY}"
"#;

/// Promotion to GitHub Package Registry.
///
/// The registry generates no provenance attestation, so the recipe does not ask
/// for one it could not then record as an attached component.
const NPM_PUBLISH_GITHUB: &str = r#"      @ENVVAR@TARBALL="$(find "${@ENVVAR@SUBJECT}" -maxdepth 1 -name '*.tgz' -print -quit)"
      test -n "${@ENVVAR@TARBALL}"
      @ENVVAR@npm_holds "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" >/dev/null \
        && @ENVVAR@HELD=0 || @ENVVAR@HELD=$?
      case "${@ENVVAR@HELD}" in
        0)
          echo "the destination already holds this version; the readback decides whether it is this release"
          exit 0
          ;;
        1) ;;
        *)
          echo "the registry did not answer whether it holds this version; refusing to submit" >&2
          exit 1
          ;;
      esac
      npm publish "${@ENVVAR@TARBALL}" --registry "${@ENVVAR@REGISTRY}"
"#;

/// Destination readback and the bounded wait it runs under.
///
/// The chain this establishes is what lets the observation say the retrieved
/// bytes are the published subject: the tarball the build produced, the
/// integrity the registry publishes, and the bytes a client with an empty cache
/// receives are compared as one value. A registry that has not indexed the
/// release yet is `pending` and one holding different bytes is `conflict`;
/// neither is decided here.
const NPM_READBACK: &str = r#"      mkdir -p "${@ENVVAR@WORK}"
      @ENVVAR@TARBALL="$(find "${@ENVVAR@SUBJECT}" -maxdepth 1 -name '*.tgz' -print -quit)"
      test -n "${@ENVVAR@TARBALL}"
      @ENVVAR@LOCAL="sha512-$(openssl dgst -sha512 -binary "${@ENVVAR@TARBALL}" | base64 -w0)"
      @ENVVAR@DESTINATION_DIGEST=""
      @ENVVAR@ELAPSED=0
      while : ; do
        @ENVVAR@DESTINATION_DIGEST="$(@ENVVAR@npm_holds \
          "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" || true)"
        if [ -n "${@ENVVAR@DESTINATION_DIGEST}" ]; then break; fi
        if [ "${@ENVVAR@ELAPSED}" -ge "${@ENVVAR@DEADLINE}" ]; then break; fi
        sleep "${@ENVVAR@INTERVAL}"
        @ENVVAR@ELAPSED=$(( @ENVVAR@ELAPSED + @ENVVAR@INTERVAL ))
        @ENVVAR@INTERVAL=$(( @ENVVAR@INTERVAL * @ENVVAR@BACKOFF ))
        if [ "${@ENVVAR@INTERVAL}" -gt "${@ENVVAR@MAXIMUM_INTERVAL}" ]; then
          @ENVVAR@INTERVAL="${@ENVVAR@MAXIMUM_INTERVAL}"
        fi
      done
      if [ -z "${@ENVVAR@DESTINATION_DIGEST}" ]; then
        @ENVVAR@observe_state pending
        exit 0
      fi
      if [ "${@ENVVAR@DESTINATION_DIGEST}" != "${@ENVVAR@LOCAL}" ]; then
        @ENVVAR@observe_state conflict \
          "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION} publishes integrity ${@ENVVAR@DESTINATION_DIGEST}, not the promoted ${@ENVVAR@LOCAL}"
        exit 0
      fi
      rm -rf "${@ENVVAR@WORK}/clean"
      mkdir -p "${@ENVVAR@WORK}/clean"
"#;

/// Clean-client retrieval and the observation it completes.
///
/// The retrieval runs under a scratch npm configuration rather than the job's
/// own. The bootstrap path writes an auth token into the user configuration and
/// it persists for the rest of the job, so a retrieval reading that file would
/// send a credential while the observation recorded a public retrieval. The
/// mode field states what happened, so the retrieval is made to be what the
/// field says.
const NPM_RETRIEVE_PUBLIC: &str = r#"      : > "${@ENVVAR@WORK}/clean/npmrc"
"#;

/// The scratch configuration a destination without anonymous read retrieves under.
///
/// The credential is the one the destination always requires of every consumer,
/// which is what `authenticated-registry` records. Writing it into the scratch
/// file rather than inheriting the job's keeps the retrieval's identity the one
/// this step chose.
const NPM_RETRIEVE_AUTHENTICATED: &str = r#"      @ENVVAR@HOST="${@ENVVAR@REGISTRY#https://}"
      printf '//%s/:_authToken=%s\n' "${@ENVVAR@HOST%/}" "${@ENVVAR@GITHUB_PACKAGES_TOKEN}" \
        > "${@ENVVAR@WORK}/clean/npmrc"
"#;

/// Retrieval, comparison, and the present observation the recipe writes.
const NPM_RETRIEVE: &str = r#"      ( cd "${@ENVVAR@WORK}/clean" \
        && npm_config_userconfig="${@ENVVAR@WORK}/clean/npmrc" \
          npm pack "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" \
          --registry "${@ENVVAR@REGISTRY}" --cache "${@ENVVAR@WORK}/clean/cache" >/dev/null )
      @ENVVAR@RETRIEVED="$(find "${@ENVVAR@WORK}/clean" -maxdepth 1 -name '*.tgz' -print -quit)"
      test -n "${@ENVVAR@RETRIEVED}"
      @ENVVAR@RETRIEVED_DIGEST="sha512-$(openssl dgst -sha512 -binary "${@ENVVAR@RETRIEVED}" | base64 -w0)"
      test "${@ENVVAR@RETRIEVED_DIGEST}" = "${@ENVVAR@LOCAL}"
      @ENVVAR@PACKAGER_VERSION="$(npm --version)"
      @ENVVAR@RETRIEVAL_VERSION="${@ENVVAR@PACKAGER_VERSION}"
      @ENVVAR@observe_present
"#;

/// One npm readback, with the retrieval identity its destination admits.
fn npm_readback(identity: &str) -> String {
    format!("{NPM_READBACK}{identity}{NPM_RETRIEVE}")
}

/// Cargo recipe: trusted publishing, a promotion gate, and cargo's own retrieval.
fn cargo_steps(context: &RecipeContext<'_>) -> Result<String, String> {
    let identity = context.publication.identity();
    let registry = context
        .publication
        .destination
        .as_deref()
        .unwrap_or(CRATES_IO);
    let origin = format!(
        "release unit {} cargo token-secret",
        context.publication.release_unit
    );
    let bootstrap = names::secret(
        context
            .unit
            .cargo
            .as_ref()
            .and_then(|cargo| cargo.token_secret.as_deref())
            .map(|value| SuppliedName {
                origin: &origin,
                value,
            })
            .as_ref(),
        CARGO_TOKEN_SECRET,
    )?;
    // Cargo names an alternate registry on the command line and reads its
    // credential from the matching environment variable, so the name and the
    // variable are derived together from the one configured destination. The
    // name reaches the scripts through `env:` and is quoted where it is used;
    // it is validated here as well, because the scripts and the name fail in
    // different ways.
    let crates_io = registry == CRATES_IO;
    let (registry_name, token_variable) = if crates_io {
        (String::new(), "CARGO_REGISTRY_TOKEN".to_owned())
    } else {
        let name = names::registry(&SuppliedName {
            origin: &format!(
                "release unit {} Cargo.toml package.publish",
                context.publication.release_unit
            ),
            value: registry,
        })?;
        let variable = format!("CARGO_REGISTRIES_{}_TOKEN", environment_fragment(&name));
        (name, variable)
    };
    // A crates.io retrieval records the public consumer path, so the probe's
    // environment simply does not name the publish credential. An alternate
    // registry's retrieval is the authenticated one it records, and cannot read
    // its index without that credential, so there it is named -- through `env:`
    // and read indirectly, like every other repository-derived name, rather
    // than written into the script. A derived name is still a name a repository
    // supplied, and a case transform on the way to a script is not a boundary.
    let carried = if crates_io {
        String::new()
    } else {
        token_variable.clone()
    };
    // An alternate registry resolves only if the probe is told where its index
    // is, and the probe inherits nothing. The index is therefore read here,
    // from the one file that declares it, and validated like every other value
    // that crosses from the repository into a derived script.
    let index = if crates_io {
        String::new()
    } else {
        let declared = cargo_registry_index(context.root, &registry_name).ok_or_else(|| {
            format!(
                "Cargo registry {registry_name:?} declares no index in .cargo/config.toml; a maintained recipe resolves an alternate registry through the index that file names"
            )
        })?;
        names::registry_index(&SuppliedName {
            origin: &format!(".cargo/config.toml registries.{registry_name}.index"),
            value: &declared,
        })?
    };
    let registry_environment = format!(
        "      @ENVVAR@REGISTRY: {}\n      @ENVVAR@REGISTRY_NAME: {}\n      @ENVVAR@CARRIED_TOKEN: {}\n      @ENVVAR@REGISTRY_INDEX_VARIABLE: {}\n      @ENVVAR@REGISTRY_INDEX_URL: {}\n",
        scalar(registry),
        scalar(&registry_name),
        scalar(&carried),
        // Named only where there is a registry to name. crates.io resolves
        // through cargo's own default, so a variable derived for it would be
        // an empty registry's spelling rather than anything cargo reads.
        scalar(&if crates_io {
            String::new()
        } else {
            format!(
                "CARGO_REGISTRIES_{}_INDEX",
                environment_fragment(&registry_name)
            )
        }),
        scalar(&index),
    );
    let mut steps = String::new();

    steps.push_str(&format!(
        "  - name: {}\n    env:\n{registry_environment}      @ENVVAR@BOOTSTRAP_TOKEN: ${{{{ secrets.{bootstrap} }}}}\n      @ENVVAR@TOKEN_VARIABLE: {}\n{}    run: |\n{}{}{}",
        scalar(&format!("Authenticate the {identity} publication")),
        scalar(&token_variable),
        subject_environment(context),
        STRICT_MODE,
        if crates_io { CARGO_RESOLVE } else { "" },
        if crates_io {
            CARGO_TRUSTED_AUTHENTICATION
        } else {
            CARGO_TOKEN_AUTHENTICATION
        },
    ));

    steps.push_str(&format!(
        "  - name: {}\n    working-directory: {}\n    env:\n{registry_environment}{}    run: |\n{}{}{}",
        scalar(&format!("Publish {identity}")),
        scalar(context.working_directory),
        subject_environment(context),
        STRICT_MODE,
        CARGO_RESOLVE,
        CARGO_PUBLISH,
    ));

    steps.push_str(&format!(
        "  - name: {}\n    env:\n{registry_environment}      @ENVVAR@DESTINATION: {}\n{}{}{}    run: |\n{}{}{}{}",
        scalar(&format!("Read {identity} back and retrieve it")),
        scalar(registry),
        subject_environment(context),
        observation_environment(context, "cargo-crate", "cargo", "cargo"),
        policy_environment(context.publication.publisher),
        STRICT_MODE,
        OBSERVE,
        CARGO_RESOLVE,
        CARGO_READBACK,
    ));
    Ok(steps)
}

/// Shell that resolves one exact crate release through cargo's own index.
///
/// Cargo has no upload-only client and no query command every supported
/// registry answers, so resolution is what stands in for both: a scratch crate
/// that depends on the exact version resolves only once the registry has
/// indexed it, and fetching that dependency downloads the published `.crate`
/// through the same path a consumer uses.
///
/// The probe answers a question that decides whether a long-lived credential is
/// reached and whether an immutable version is submitted, so what it reads has
/// to be what derivation chose. Cargo reads from two places, and both were
/// routes into that decision: a configuration file discovered by walking up
/// from the working directory, and the process environment. Four keys across
/// those two — `[net] offline`, `[source] replace-with`,
/// `[registries.crates-io] index`, and the `CARGO_REGISTRIES_*_INDEX` variable
/// a repository can set in its own workflow's top-level `env:` — each produce
/// the same misclassification by a different route, and each was closed
/// separately before this.
///
/// So the probe is given an environment rather than allowed to inherit one.
/// `env -i` clears it and only [`INHERITED_ENVIRONMENT`] is put back, with the
/// derived entries beside it: a variable nobody thought of cannot arrive, which
/// is the property route-by-route closure never had.
///
/// The inherited members carry the runner's values, and a repository could
/// otherwise choose those values through the top-level `env:` block
/// convergence preserves — which would make the client that answers the probe
/// a repository's choice, since every one of them takes part in finding it.
/// That is why convergence refuses a workflow declaring any of these names:
/// the list is the whole of what is inherited, and refusing the names is what
/// makes them the runner's rather than the repository's.
///
/// The publish credential is simply not in the list where the retrieval records
/// the public consumer path, so withholding it is no longer a step. A
/// destination whose recipe records an authenticated retrieval has its token in
/// the list, because there the consumer path is the authenticated one and the
/// index cannot be read without it.
///
/// The three outcomes are distinct. Cargo reports a crate the index does not
/// carry differently from a fetch it could not perform, and only the first is
/// evidence of absence; treating both as absence routes a transient failure
/// into the bootstrap credential. The network is forced on for the same reason
/// the environment is cleared: an offline resolution reports absence in the
/// words absence uses, so the condition is removed rather than parsed.
const CARGO_RESOLVE: &str = r#"      @ENVVAR@REGISTRY_ARGUMENTS=()
      @ENVVAR@ALLOWED=(@INHERITED@ CARGO_NET_OFFLINE=false)
      if [ -n "${@ENVVAR@REGISTRY_NAME:-}" ]; then
        @ENVVAR@REGISTRY_ARGUMENTS=(--registry "${@ENVVAR@REGISTRY_NAME}")
        @ENVVAR@ALLOWED+=("${@ENVVAR@REGISTRY_INDEX_VARIABLE}=${@ENVVAR@REGISTRY_INDEX_URL}")
      fi
      if [ -n "${@ENVVAR@CARRIED_TOKEN:-}" ] && [ -n "${!@ENVVAR@CARRIED_TOKEN:-}" ]; then
        @ENVVAR@ALLOWED+=("${@ENVVAR@CARRIED_TOKEN}=${!@ENVVAR@CARRIED_TOKEN}")
      fi
      @ENVVAR@resolve() {
        rm -rf "$1"
        mkdir -p "$1"
        env -i "${@ENVVAR@ALLOWED[@]}" CARGO_HOME="$1/home" \
          cargo new --quiet --lib "$1/probe" >/dev/null
        if ( cd "$1/probe" \
          && env -i "${@ENVVAR@ALLOWED[@]}" CARGO_HOME="$1/home" \
            cargo add --quiet "${@ENVVAR@REGISTRY_ARGUMENTS[@]}" \
            "${@ENVVAR@SUBJECT_IDENTITY}@=${@ENVVAR@VERSION}" \
          && env -i "${@ENVVAR@ALLOWED[@]}" CARGO_HOME="$1/home" \
            cargo fetch --quiet ) > "$1/log" 2>&1; then
          return 0
        fi
        if grep -qiE 'could not be found|not found in registry|no matching package' "$1/log"; then
          return 1
        fi
        cat "$1/log" >&2
        return 2
      }
"#;

/// crates.io trusted publishing with a protected first-publication path.
///
/// The registry exchanges a workflow OpenID Connect token for a short-lived
/// publish token, and it can only do that for a crate a trusted publisher is
/// already configured on. A crate the registry does not hold yet therefore has
/// no trusted identity to present, which is the one case the bootstrap token
/// covers. A probe that did not answer is not that case: it fails the job
/// rather than reaching for the token.
const CARGO_TRUSTED_AUTHENTICATION: &str = r#"      @ENVVAR@resolve "${RUNNER_TEMP}/@JOB@bootstrap-probe" && @ENVVAR@HELD=0 || @ENVVAR@HELD=$?
      case "${@ENVVAR@HELD}" in
        0) ;;
        1)
          if [ -z "${@ENVVAR@BOOTSTRAP_TOKEN:-}" ]; then
            echo "${@ENVVAR@SUBJECT_IDENTITY} does not exist yet and no bootstrap token secret is available" >&2
            echo "a trusted publisher can only be configured for an existing crate" >&2
            exit 1
          fi
          echo "${@ENVVAR@TOKEN_VARIABLE}=${@ENVVAR@BOOTSTRAP_TOKEN}" >> "${GITHUB_ENV}"
          exit 0
          ;;
        *)
          echo "the registry did not answer whether it holds ${@ENVVAR@SUBJECT_IDENTITY}" >&2
          echo "a bootstrap token is reachable only on a proven first publication" >&2
          exit 1
          ;;
      esac
      @ENVVAR@JWT="$(curl --fail --silent --show-error \
        --header "Authorization: bearer ${ACTIONS_ID_TOKEN_REQUEST_TOKEN}" \
        "${ACTIONS_ID_TOKEN_REQUEST_URL}&audience=crates.io" | jq -r '.value')"
      test -n "${@ENVVAR@JWT}"
      @ENVVAR@TOKEN="$(jq -n --arg jwt "${@ENVVAR@JWT}" '{jwt: $jwt}' | curl --fail --silent \
        --show-error --request PUT --header 'Content-Type: application/json' --data @- \
        "https://crates.io/api/v1/trusted_publishing/tokens" | jq -r '.token')"
      test -n "${@ENVVAR@TOKEN}"
      echo "::add-mask::${@ENVVAR@TOKEN}"
      echo "${@ENVVAR@TOKEN_VARIABLE}=${@ENVVAR@TOKEN}" >> "${GITHUB_ENV}"
"#;

/// Token authentication for a configured registry that is not crates.io.
///
/// An alternate registry defines its own trusted-publishing support, if any, so
/// the maintained recipe uses the credential every such registry does accept
/// and leaves the name of the secret holding it to configuration.
const CARGO_TOKEN_AUTHENTICATION: &str = r#"      if [ -z "${@ENVVAR@BOOTSTRAP_TOKEN:-}" ]; then
        echo "publishing to ${@ENVVAR@REGISTRY} requires a registry token secret" >&2
        exit 1
      fi
      echo "${@ENVVAR@TOKEN_VARIABLE}=${@ENVVAR@BOOTSTRAP_TOKEN}" >> "${GITHUB_ENV}"
"#;

/// Promotion gate and publication for one Cargo destination.
///
/// Cargo has no command that uploads an existing `.crate`, so publication
/// necessarily re-packages the release-unit sources, and `cargo publish`
/// packages once more of its own. What this gate proves is therefore
/// reproducibility rather than identity: packaging the same sources twice in
/// this job produced the same bytes as the build job's subject, so the third
/// packaging inside `cargo publish` produces them too. The claim is closed on
/// the other side by the readback, which compares the checksum the registry
/// publishes against the sealed subject and records a conflict if they differ.
/// Both halves are needed; the gate alone would still be one packaging short.
const CARGO_PUBLISH: &str = r#"      @ENVVAR@CRATE="$(find "${@ENVVAR@SUBJECT}" -maxdepth 1 -name '*.crate' -print -quit)"
      test -n "${@ENVVAR@CRATE}"
      @ENVVAR@resolve "${RUNNER_TEMP}/@JOB@publish-probe" && @ENVVAR@HELD=0 || @ENVVAR@HELD=$?
      case "${@ENVVAR@HELD}" in
        0)
          echo "the destination already holds this version; the readback decides whether it is this release"
          exit 0
          ;;
        1) ;;
        *)
          echo "the registry did not answer whether it holds this version; refusing to submit" >&2
          exit 1
          ;;
      esac
      cargo package --locked --no-verify --target-dir "${RUNNER_TEMP}/@JOB@repackage"
      @ENVVAR@REPACKAGED="${RUNNER_TEMP}/@JOB@repackage/package/$(basename "${@ENVVAR@CRATE}")"
      test "$(sha256sum < "${@ENVVAR@REPACKAGED}" | cut -d' ' -f1)" \
        = "$(sha256sum < "${@ENVVAR@CRATE}" | cut -d' ' -f1)"
      cargo publish --locked --no-verify "${@ENVVAR@REGISTRY_ARGUMENTS[@]}"
"#;

/// Destination readback, clean-client retrieval, and the observation they produce.
///
/// One cargo resolution is both: the registry index resolving the exact version
/// is the readback, and the `.crate` that resolution fetches into an empty
/// `CARGO_HOME` is the retrieval. The checksum cargo records in the scratch
/// lock file is the destination's own claim about those bytes, and comparing it
/// with the bytes on disk is what establishes that the release a consumer
/// receives is the crate this job promoted.
const CARGO_READBACK: &str = r#"      mkdir -p "${@ENVVAR@WORK}"
      @ENVVAR@CRATE="$(find "${@ENVVAR@SUBJECT}" -maxdepth 1 -name '*.crate' -print -quit)"
      test -n "${@ENVVAR@CRATE}"
      @ENVVAR@LOCAL="$(sha256sum < "${@ENVVAR@CRATE}" | cut -d' ' -f1)"
      @ENVVAR@ELAPSED=0
      @ENVVAR@RESOLVED=no
      while : ; do
        if @ENVVAR@resolve "${@ENVVAR@WORK}/clean"; then @ENVVAR@RESOLVED=yes; break; fi
        if [ "${@ENVVAR@ELAPSED}" -ge "${@ENVVAR@DEADLINE}" ]; then break; fi
        sleep "${@ENVVAR@INTERVAL}"
        @ENVVAR@ELAPSED=$(( @ENVVAR@ELAPSED + @ENVVAR@INTERVAL ))
        @ENVVAR@INTERVAL=$(( @ENVVAR@INTERVAL * @ENVVAR@BACKOFF ))
        if [ "${@ENVVAR@INTERVAL}" -gt "${@ENVVAR@MAXIMUM_INTERVAL}" ]; then
          @ENVVAR@INTERVAL="${@ENVVAR@MAXIMUM_INTERVAL}"
        fi
      done
      if [ "${@ENVVAR@RESOLVED}" != yes ]; then
        @ENVVAR@observe_state pending
        exit 0
      fi
      @ENVVAR@DESTINATION_DIGEST="$(sed -n "/^name = \"${@ENVVAR@SUBJECT_IDENTITY}\"$/,/^$/p" \
        "${@ENVVAR@WORK}/clean/probe/Cargo.lock" | sed -n 's/^checksum = "\(.*\)"$/\1/p')"
      test -n "${@ENVVAR@DESTINATION_DIGEST}"
      @ENVVAR@RETRIEVED="$(find "${@ENVVAR@WORK}/clean/home/registry/cache" -type f \
        -name "${@ENVVAR@SUBJECT_IDENTITY}-${@ENVVAR@VERSION}.crate" -print -quit)"
      test -n "${@ENVVAR@RETRIEVED}"
      @ENVVAR@RETRIEVED_DIGEST="$(sha256sum < "${@ENVVAR@RETRIEVED}" | cut -d' ' -f1)"
      if [ "${@ENVVAR@DESTINATION_DIGEST}" != "${@ENVVAR@LOCAL}" ]; then
        @ENVVAR@observe_state conflict \
          "${@ENVVAR@SUBJECT_IDENTITY} ${@ENVVAR@VERSION} publishes checksum ${@ENVVAR@DESTINATION_DIGEST}, not the promoted ${@ENVVAR@LOCAL}"
        exit 0
      fi
      test "${@ENVVAR@RETRIEVED_DIGEST}" = "${@ENVVAR@LOCAL}"
      @ENVVAR@PACKAGER_VERSION="$(cargo --version | cut -d' ' -f2)"
      @ENVVAR@RETRIEVAL_VERSION="${@ENVVAR@PACKAGER_VERSION}"
      @ENVVAR@observe_present
"#;

/// Repository variable naming the Docker Hub account a recipe authenticates as.
const DOCKERHUB_USERNAME_VAR: &str = "DOCKERHUB_USERNAME";
/// Conventional GitHub secret holding the Docker Hub access token.
const DOCKERHUB_TOKEN_SECRET: &str = "DOCKERHUB_TOKEN";
/// Dev Container CLI revision the maintained recipe drives.
const DEV_CONTAINER_CLI: &str = "@devcontainers/cli@0.88.0";

/// Credential and promotion steps one OCI destination requires.
///
/// A maintained OCI recipe owns everything that happens at a destination:
/// authentication, promotion of the bytes the build job produced, alias
/// mutation, attached components, destination readback, the closure-time
/// consumer retrieval, and the observation those steps leave behind.
///
/// The build job already sealed the subject, so no recipe here builds. Both
/// packagers promote what the seal produced and then prove the destination
/// resolved it: the Buildx recipe pushes the sealed layout itself, and the Dev
/// Container recipe drives a native client that publishes from source and is
/// therefore held to the packaged bytes afterwards.
fn oci_steps(context: &RecipeContext<'_>) -> Result<String, StepsRefusal> {
    let publication = context.publication;
    let identity = publication.identity();
    // A Dev Container Feature is namespaced by the repository that publishes
    // it: its client resolves `<owner>/<repository>/<feature id>`, which a
    // two-segment repository override cannot express and which the client would
    // ignore. Accepting one would point every readback at a repository nothing
    // was written to, so the combination is refused where it is configured
    // rather than after the destination has been mutated.
    if publication.packager == Packager::DevContainerCli && publication.destination.is_some() {
        return Err(StepsRefusal::not_overridable(
            format!(
                "publication {identity} configures a repository, but a Dev Container Feature is namespaced by the repository publishing it and its client resolves owner/repository/{}",
                context.subject_identity
            ),
            format!(
                "release-units.{}.oci.{}.repository",
                publication.release_unit, publication.target
            ),
        ));
    }
    oci_destination_steps(context).map_err(|message| StepsRefusal::underivable(&identity, &message))
}

fn oci_destination_steps(context: &RecipeContext<'_>) -> Result<String, String> {
    let publication = context.publication;
    let identity = publication.identity();
    let feature = publication.packager == Packager::DevContainerCli;
    let signed = publication
        .components
        .contains(&AttachedComponent::Signature);

    // Every client is installed by a pinned, credential-free step. The Dev
    // Container CLI comes from npm, which runs package lifecycle scripts, so it
    // is installed here rather than in the step that holds the registry token:
    // an install executing arbitrary code beside a credential would give back
    // exactly what these steps are for. Scripts are refused because this client
    // needs none.
    let mut steps =
        format!("  - name: Install the registry client\n    uses: {SETUP_CRANE_ACTION}\n");
    if feature {
        steps.push_str(&format!(
            "  - name: Install the Dev Container client\n    run: npm install --global --no-fund --no-audit --ignore-scripts {DEV_CONTAINER_CLI}\n"
        ));
    }
    if signed {
        steps.push_str(&format!(
            "  - name: Install the keyless signing client\n    uses: {COSIGN_INSTALLER_ACTION}\n"
        ));
    }

    let destination = oci_destination_environment(context)?;
    let components = publication
        .components
        .iter()
        .map(|component| component.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let namespace = if feature {
        "      @ENVVAR@FEATURE_NAMESPACE: \"${{ github.repository }}\"\n".to_owned()
    } else {
        String::new()
    };
    let (kind, packager_version_command) = if feature {
        ("dev-container-feature", "devcontainer")
    } else {
        ("oci-image", "docker buildx")
    };
    let promote = if feature {
        [
            OCI_EXISTING,
            DEV_CONTAINER_CONFLICT_GATE,
            OCI_CONFLICT_REPORT,
            DEV_CONTAINER_CONFLICT_TAIL,
            DEV_CONTAINER_PUBLISH,
        ]
        .concat()
    } else {
        [
            OCI_IMAGE_PROMOTE_HEAD,
            OCI_EXISTING,
            OCI_IMAGE_CONFLICT_GATE,
            OCI_CONFLICT_REPORT,
            OCI_IMAGE_PROMOTE_TAIL,
        ]
        .concat()
    };
    steps.push_str(&format!(
        "  - name: {}\n    working-directory: {}\n    env:\n{}{}{}{}      @ENVVAR@COMPONENTS: {}\n{namespace}    run: |\n{}{}{}{}{}{}{}{}{}",
        scalar(&format!("Publish {identity}")),
        scalar(context.working_directory),
        subject_environment(context),
        observation_environment(
            context,
            kind,
            publication.packager.as_str(),
            "crane",
        ),
        policy_environment(publication.publisher),
        destination,
        scalar(&components),
        STRICT_MODE,
        OBSERVE,
        OCI_PROLOGUE,
        promote,
        OCI_ALIAS_ENTITLEMENT,
        if feature { "" } else { OCI_ALIAS_PROMOTE },
        OCI_ALIAS_READBACK,
        oci_attached_components(publication),
        oci_observation(publication, packager_version_command),
    ));
    Ok(steps)
}

/// Destination-specific values one OCI recipe body reads from its environment.
///
/// Every value a recipe interpolates is routed through `env:` rather than
/// spliced into the shell source, so a configured repository or a credential
/// name can never become executable text in a step that holds a registry token.
/// The credential names are held to a GitHub secret identifier first, because
/// they land in `${{ secrets.NAME }}`, which is an identifier position: a name
/// carrying a bracket would not name a missing secret, it would change what the
/// expression evaluates.
fn oci_destination_environment(context: &RecipeContext<'_>) -> Result<String, String> {
    let publication = context.publication;
    let oci = context.unit.oci.as_ref();
    let (registry, destination, user, token) = match publication.target.as_str() {
        "dockerhub" => {
            let target = oci.and_then(|oci| oci.dockerhub.as_ref());
            let origin = format!(
                "release unit {} oci dockerhub username-var",
                publication.release_unit
            );
            let username = names::secret(
                target
                    .and_then(|target| target.username_var.as_deref())
                    .map(|value| SuppliedName {
                        origin: &origin,
                        value,
                    })
                    .as_ref(),
                DOCKERHUB_USERNAME_VAR,
            )?;
            let origin = format!(
                "release unit {} oci dockerhub token-secret",
                publication.release_unit
            );
            let secret = names::secret(
                target
                    .and_then(|target| target.token_secret.as_deref())
                    .map(|value| SuppliedName {
                        origin: &origin,
                        value,
                    })
                    .as_ref(),
                DOCKERHUB_TOKEN_SECRET,
            )?;
            (
                "docker.io".to_owned(),
                publication.destination.clone().unwrap_or_default(),
                format!("${{{{ vars.{username} }}}}"),
                format!("${{{{ secrets.{secret} }}}}"),
            )
        }
        // GHCR derives its owner from GitHub and its subject from the name the
        // source declared, so an empty mapping still resolves a destination. A
        // Dev Container Feature is namespaced by the repository because that is
        // where its native client resolves it from.
        _ => {
            let derived = if publication.packager == Packager::DevContainerCli {
                format!("${{{{ github.repository }}}}/{}", context.subject_identity)
            } else {
                format!(
                    "${{{{ github.repository_owner }}}}/{}",
                    context.subject_identity
                )
            };
            (
                "ghcr.io".to_owned(),
                publication.destination.clone().unwrap_or(derived),
                "${{ github.actor }}".to_owned(),
                "${{ secrets.GITHUB_TOKEN }}".to_owned(),
            )
        }
    };
    Ok(format!(
        "      @ENVVAR@REGISTRY: {}\n      @ENVVAR@DESTINATION: {}\n      @ENVVAR@REGISTRY_USER: {}\n      @ENVVAR@REGISTRY_TOKEN: {}\n",
        scalar(&registry),
        scalar(&destination),
        scalar(&user),
        scalar(&token),
    ))
}

/// Values every OCI recipe body starts from, and the registry session it opens.
///
/// The version and the sealed digest are the build job's outputs rather than
/// values this body re-derives, so a recipe cannot record its publication under
/// another release's version or against bytes it did not promote.
const OCI_PROLOGUE: &str = r#"      version="${@ENVVAR@VERSION}"
      subject_digest="${@ENVVAR@SUBJECT_DIGEST}"
      test -n "${version}"
      test -n "${subject_digest}"
      repository="${@ENVVAR@REGISTRY}/${@ENVVAR@DESTINATION}"
      mkdir -p "${@ENVVAR@WORK}"
      aliases_file="${@ENVVAR@WORK}/aliases.yml"
      metadata_file="${@ENVVAR@WORK}/metadata.yml"
      : > "${aliases_file}"
      : > "${metadata_file}"
      printf '%s' "${@ENVVAR@REGISTRY_TOKEN}" | crane auth login "${@ENVVAR@REGISTRY}" \
        --username "${@ENVVAR@REGISTRY_USER}" --password-stdin
"#;

/// Read what the destination already holds under the released version.
///
/// Existence is decided from the destination's tag listing rather than from a
/// swallowed digest read, so an authentication or registry failure at the
/// digest read itself is no longer indistinguishable from an absent tag. What
/// remains swallowed is the listing of a repository that does not exist yet,
/// which is the same reading as a repository carrying no versions; a genuine
/// outage there fails loudly at the promotion immediately after.
const OCI_EXISTING: &str = r#"      known_tags="$(crane ls "${repository}" 2>/dev/null || true)"
      existing=""
      if printf '%s\n' "${known_tags}" | grep -Fxq "${version}"; then
        existing="$(crane digest "${repository}:${version}")"
      fi
"#;

/// Report a destination holding another subject, without touching it.
const OCI_CONFLICT_REPORT: &str = r#"        @ENVVAR@observe_state conflict "$(printf '%s already holds %s under version %s, which is not the subject this release built' \
          "${repository}" "${conflicting}" "${version}")"
        exit 0
"#;

/// Push the sealed layout and prove the destination holds what it sealed.
///
/// The identity compared is the set of manifests the index references, not the
/// index digest. A registry re-serializes the index it is given, so its digest
/// is a property of the bytes that landed rather than of the layout, and
/// asserting the layout's own index digest would fail every real publication
/// while proving nothing extra. The referenced manifests are carried through
/// unchanged, so comparing them is what establishes that this destination holds
/// the subject this release built -- and, because every destination is given
/// the same layout, that all of them hold the same one.
///
/// The digest the destination reports is separately held to its own bytes, so a
/// destination that names one subject and serves another is caught rather than
/// recorded. A destination already holding different content under this version
/// is reported as a conflict instead of being overwritten.
const OCI_IMAGE_PROMOTE_HEAD: &str = r#"      layout="${@ENVVAR@WORK}/layout"
      rm -rf "${layout}"
      mkdir -p "${layout}"
      tar -xf "${@ENVVAR@SUBJECT}/subject.oci.tar" -C "${layout}"
      sealed_index="$(jq -r '.manifests[0].digest' "${layout}/index.json")"
      sealed_manifests="$(jq -S -r '[.manifests[].digest] | sort | .[]' \
        "${layout}/blobs/sha256/${sealed_index#sha256:}")"
"#;

const OCI_IMAGE_CONFLICT_GATE: &str = r#"      existing_manifests=""
      if [ -n "${existing}" ]; then
        existing_manifests="$(crane manifest "${repository}@${existing}" \
          | jq -S -r '[.manifests[]?.digest] | sort | .[]')"
      fi
      if [ -n "${existing}" ] && [ "${existing_manifests}" != "${sealed_manifests}" ]; then
        conflicting="${existing}"
"#;

const OCI_IMAGE_PROMOTE_TAIL: &str = r#"      fi
      crane push --index "${layout}" "${repository}:${version}"
      published="$(crane digest "${repository}:${version}")"
      index="$(crane manifest "${repository}@${published}")"
      test "$(printf '%s' "${index}" | jq -S -r '[.manifests[].digest] | sort | .[]')" \
        = "${sealed_manifests}"
      annotated="$(printf '%s' "${index}" \
        | jq -r '.annotations["org.opencontainers.image.version"] // ""')"
      test "${annotated}" = "${version}"
      named="$(printf '%s' "${index}" \
        | jq -r '.annotations["org.opencontainers.image.title"] // ""')"
      test "${named}" = "${@ENVVAR@SUBJECT_IDENTITY}"
"#;

/// Refuse to hand a destination holding another Feature to the native client.
///
/// The client publishes from source and would overwrite the tag, and the layer
/// comparison afterwards cannot notice: it compares against the bytes that same
/// client just pushed. The decidable question before it runs is whether the
/// destination's existing layer is the one the build job sealed, which the
/// packaged artifact answers without the client's help.
const DEV_CONTAINER_CONFLICT_GATE: &str = r#"      packaged="sha256:$(sha256sum "${@ENVVAR@SUBJECT}/devcontainer-feature-${@ENVVAR@SUBJECT_IDENTITY}.tgz" \
        | cut -d ' ' -f 1)"
      if [ -n "${existing}" ]; then
        existing_layer="$(crane manifest "${repository}@${existing}" | jq -r '.layers[0].digest')"
      else
        existing_layer="${packaged}"
      fi
      if [ "${existing_layer}" != "${packaged}" ]; then
        conflicting="${existing}"
"#;

const DEV_CONTAINER_CONFLICT_TAIL: &str = r#"      fi
"#;

/// Publish a Feature through its native client and prove it promoted the seal.
const DEV_CONTAINER_PUBLISH: &str = r#"      devcontainer features publish --namespace "${@ENVVAR@FEATURE_NAMESPACE}" .
      published="$(crane digest "${repository}:${version}")"
      layer="$(crane manifest "${repository}@${published}" | jq -r '.layers[0].digest')"
      test "${layer}" = "${packaged}"
"#;

/// Decide which stable aliases the released version is entitled to.
///
/// Entitlement is decided from the versions the destination already carries
/// rather than from a local comparison, so a backport publishing 1.2.4 after
/// 1.3.0 exists is entitled to `1.2` and nothing else, and a rerun of any
/// release reaches the same decision. Major zero has no broad alias.
///
/// The compared set is the stable versions the destination carries, which is
/// also what keeps a prerelease off every stable alias: a version carrying a
/// prerelease identifier is never a member of that set, so it is never the
/// newest member of one. Restating that as its own branch would add a
/// condition nothing could ever falsify.
const OCI_ALIAS_ENTITLEMENT: &str = r#"      aliases=""
      core="${version%%-*}"
      major="${core%%.*}"
      minor="${core%.*}"
      minor_pattern="${minor//./\.}"
      major_pattern="${major//./\.}"
      published_tags="$(crane ls "${repository}" | grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' || true)"
      newest="$(printf '%s\n' "${published_tags}" | sort -V | tail -n 1)"
      if [ "${newest}" = "${version}" ]; then
        aliases="latest"
      fi
      newest_minor="$(printf '%s\n' "${published_tags}" \
        | grep -E "^${minor_pattern}\.[0-9]+$" | sort -V | tail -n 1 || true)"
      if [ "${newest_minor}" = "${version}" ]; then
        aliases="${aliases} ${minor}"
      fi
      if [ "${major}" != "0" ]; then
        newest_major="$(printf '%s\n' "${published_tags}" \
          | grep -E "^${major_pattern}\.[0-9]+\.[0-9]+$" | sort -V | tail -n 1 || true)"
        if [ "${newest_major}" = "${version}" ]; then
          aliases="${aliases} ${major}"
        fi
      fi
"#;

/// Move the aliases this release is entitled to.
const OCI_ALIAS_PROMOTE: &str = r#"      for alias in ${aliases}; do
        crane tag "${repository}:${version}" "${alias}"
      done
"#;

/// Compare every mutable alias the destination now resolves with entitlement.
///
/// Both directions are checked, and that is the point. An entitled alias that
/// does not resolve the published subject means the promotion did not take. An
/// alias that does resolve it without being entitled means something moved a
/// stable alias this release had no right to -- which is the only way to hold a
/// native client that maintains its own tags to the same rule, rather than
/// predicting what it will do and recording the prediction as an observation.
const OCI_ALIAS_READBACK: &str = r#"      for alias in latest "${minor}" "${major}"; do
        alias_digest="$(crane digest "${repository}:${alias}" 2>/dev/null || true)"
        entitled=""
        case " ${aliases} " in
          *" ${alias} "*) entitled="yes" ;;
        esac
        if [ "${alias_digest}" = "${published}" ]; then
          test -n "${entitled}"
          printf -- '  - name: "%s"\n    digest: "%s"\n' "${alias}" "${alias_digest}" \
            >> "${aliases_file}"
        else
          test -z "${entitled}"
        fi
      done
"#;

/// Attach and read back exactly the components this target did not omit.
///
/// The body carries an arm only for a component this target still selects, so
/// an omitted component leaves no trace in the recipe at all: nothing attaches
/// it, nothing looks for it, and nothing records that it was left out. A
/// selected component that the destination does not hold fails the publication
/// instead of quietly dropping out of affirmative evidence.
fn oci_attached_components(publication: &SelectedPublication) -> String {
    if publication.components.is_empty() {
        return String::new();
    }
    let mut body = String::new();
    let reads_attestation = publication.components.iter().any(|component| {
        matches!(
            component,
            AttachedComponent::Sbom | AttachedComponent::Provenance
        )
    });
    if reads_attestation {
        body.push_str(OCI_ATTESTATION_READ);
    }
    if publication
        .components
        .contains(&AttachedComponent::Provenance)
    {
        body.push_str("      provenance_digest=\"\"\n");
    }
    body.push_str(
        "      for component in ${@ENVVAR@COMPONENTS}; do\n        case \"${component}\" in\n",
    );
    for component in &publication.components {
        body.push_str(match component {
            AttachedComponent::Sbom => OCI_SBOM_ARM,
            AttachedComponent::Provenance => OCI_PROVENANCE_ARM,
            AttachedComponent::Signature => OCI_SIGNATURE_ARM,
        });
    }
    body.push_str(OCI_COMPONENT_RECORD);
    body
}

/// Locate the attestation manifest the packager attached to the subject.
const OCI_ATTESTATION_READ: &str = r#"      attestation="$(crane manifest "${repository}@${published}" \
        | jq -r 'first(.manifests[]? | select(.annotations["vnd.docker.reference.type"] == "attestation-manifest") | .digest) // ""')"
      predicates=""
      if [ -n "${attestation}" ]; then
        predicates="$(crane manifest "${repository}@${attestation}")"
      fi
"#;

const OCI_SBOM_ARM: &str = r#"          sbom)
            component_digest="$(printf '%s' "${predicates}" \
              | jq -r 'first(.layers[]? | select(.annotations["in-toto.io/predicate-type"] | test("spdx")) | .digest) // ""')"
            ;;
"#;

const OCI_PROVENANCE_ARM: &str = r#"          provenance)
            component_digest="$(printf '%s' "${predicates}" \
              | jq -r 'first(.layers[]? | select(.annotations["in-toto.io/predicate-type"] | test("slsa|provenance")) | .digest) // ""')"
            provenance_digest="${component_digest}"
            ;;
"#;

const OCI_SIGNATURE_ARM: &str = r#"          signature)
            cosign sign --yes "${repository}@${published}"
            component_digest="$(crane digest "$(cosign triangulate "${repository}@${published}")")"
            ;;
"#;

const OCI_COMPONENT_RECORD: &str = r#"          *)
            component_digest=""
            ;;
        esac
        test -n "${component_digest}"
        printf -- '  - kind: "%s"\n    digest: "%s"\n    reference: "%s"\n' \
          "${component}" "${component_digest}" "${repository}@${published}" >> "${metadata_file}"
      done
"#;

/// Retrieve the release the way an ordinary public consumer would.
///
/// The client is given an empty credential store, so it resolves the release
/// with no authority this job holds. It then retrieves the subject's own bytes
/// and checks that they hash to the digest the destination published, which is
/// what discharges the recipe's obligation to prove the retrieved bytes are the
/// published subject. Resolution alone would leave a destination that serves a
/// tag publicly but refuses its content indistinguishable from one that does
/// not -- a real state at GHCR while a package's visibility is changing.
///
/// A destination no public client can reach is reported rather than crashed
/// into. The publication was accepted; what has not happened is the
/// destination becoming observable, which is exactly the `pending` state the
/// protocol already has a bounded policy for. Dying with the client's own
/// error would leave `verify publication` with nothing to read about a
/// destination that had in fact been fully written, on what is the most likely
/// first-run outcome: a GHCR package is private until someone makes it public.
const OCI_CLEAN_CLIENT: &str = r#"      clean_client="${@ENVVAR@WORK}/clean"
      rm -rf "${clean_client}"
      mkdir -p "${clean_client}"
      if ! retrieved="$(DOCKER_CONFIG="${clean_client}" crane digest "${repository}:${version}" \
        2>/dev/null)" \
        || ! DOCKER_CONFIG="${clean_client}" crane manifest "${repository}@${retrieved}" \
          > "${clean_client}/subject.json" 2>/dev/null; then
        printf '%s carries version %s but no public client can retrieve it; an OCI package is observable to its consumers only while it is public, and a package is private when it is first pushed\n' \
          "${repository}" "${version}" >&2
        @ENVVAR@observe_state pending
        exit 0
      fi
      test "${retrieved}" = "${published}"
      test "sha256:$(sha256sum < "${clean_client}/subject.json" | cut -d ' ' -f 1)" = "${retrieved}"
      client_version="$(crane version)"
"#;

/// Compose the observation the verification command reads.
///
/// Everything the schema fixes is written by the shared `observe_present`
/// helper, so this adds only what an OCI publication has beyond a publication:
/// the components it attached, the aliases it moved, and the native build
/// provenance the packager produced.
fn oci_observation(publication: &SelectedPublication, packager_version_command: &str) -> String {
    let provenance = if publication
        .components
        .contains(&AttachedComponent::Provenance)
    {
        "      {\n        printf 'build-provenance:\\n'\n        printf -- '  - kind: \"%s\"\\n    digest: \"%s\"\\n    reference: \"%s\"\\n' \\\n          'oci-attestation' \"${provenance_digest}\" \"${repository}@${published}\"\n      } >> \"${@ENVVAR@OBSERVATION}\"\n"
    } else {
        ""
    };
    format!(
        r#"{OCI_CLEAN_CLIENT}      @ENVVAR@PACKAGER_VERSION="$({packager_version_command} version | head -n 1)"
      @ENVVAR@DESTINATION_DIGEST="${{published}}"
      @ENVVAR@RETRIEVAL_VERSION="${{client_version}}"
      @ENVVAR@RETRIEVED_DIGEST="${{retrieved}}"
      @ENVVAR@observe_present
      if [ -s "${{metadata_file}}" ]; then
        printf 'attached-metadata:\n' >> "${{@ENVVAR@OBSERVATION}}"
        cat "${{metadata_file}}" >> "${{@ENVVAR@OBSERVATION}}"
      fi
      if [ -s "${{aliases_file}}" ]; then
        printf 'destination-aliases:\n' >> "${{@ENVVAR@OBSERVATION}}"
        cat "${{aliases_file}}" >> "${{@ENVVAR@OBSERVATION}}"
      fi
{provenance}"#
    )
}
