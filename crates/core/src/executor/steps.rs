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
//! A probe that did not succeed is not an answer. Every existence check
//! separates "the destination does not hold this" from "the check did not
//! complete", because the two are one shell exit status apart and only the
//! first may reach a bootstrap credential.

use crate::config::ReleaseUnitConfig;
use crate::executor::names::{self, SuppliedName};
use crate::executor::recipe::{Packager, SelectedPublication, PRIMARY_TARGET};
use crate::executor::workflow::scalar;
use crate::model::PublisherKind;
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

/// Steps one publication's maintained recipe contributes to its publisher job.
///
/// The three packagers below each keep one arm rather than sharing a collapsed
/// one. Their recipes are owned by separate tasks working from a common base,
/// and a shared arm makes one edit each of them has to make into one edit they
/// have to make together.
pub(super) fn recipe_steps(context: &RecipeContext<'_>) -> Result<String, String> {
    match context.publication.packager {
        Packager::Npm => npm_steps(context),
        Packager::Cargo => cargo_steps(context),
        Packager::GoReleaser => Ok(promote_only(context, "goreleaser release --clean")),
        Packager::Buildx => Ok(promote_only(
            context,
            "docker buildx build --push --provenance true --sbom true .",
        )),
        Packager::DevContainerCli => Ok(promote_only(
            context,
            "devcontainer features publish --namespace \"${GITHUB_REPOSITORY}\" .",
        )),
    }
}

/// The single promotion step a recipe without derived readback still emits.
///
/// Authentication, readback and retrieval belong to the tasks that own those
/// destinations; until then the job promotes its subject with one native
/// command and writes no observation, which the verification step reports.
fn promote_only(context: &RecipeContext<'_>, command: &str) -> String {
    format!(
        "  - name: {}\n    working-directory: {}\n    env:\n      @ENVVAR@SUBJECT: ${{{{ runner.temp }}}}/@JOB@subject/bytes\n    run: {command}\n",
        scalar(&format!("Publish {}", context.publication.identity())),
        scalar(context.working_directory),
    )
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
fn observation_environment(context: &RecipeContext<'_>, kind: &str, packager: &str) -> String {
    format!(
        "      @ENVVAR@OBSERVATION: {}\n      @ENVVAR@RELEASE_UNIT: {}\n      @ENVVAR@PUBLISHER: {}\n      @ENVVAR@TARGET: {}\n      @ENVVAR@WORK: {}\n      @ENVVAR@SUBJECT_KIND: {}\n      @ENVVAR@PACKAGER_ID: {}\n      @ENVVAR@RETRIEVAL_MODE: {}\n",
        scalar(context.observation),
        scalar(&context.publication.release_unit),
        scalar(context.publication.publisher.as_str()),
        scalar(&context.publication.target),
        scalar(context.work),
        scalar(kind),
        scalar(packager),
        scalar(context.publication.retrieval.as_str()),
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
          printf '  client: "%s"\n' "${@ENVVAR@PACKAGER_ID}"
          printf '  version: "%s"\n' "${@ENVVAR@RETRIEVAL_VERSION}"
          printf '  digest: "%s"\n' "${@ENVVAR@RETRIEVED_DIGEST}"
        } > "${@ENVVAR@OBSERVATION}"
      }
"#;

/// npm recipe: trusted publishing, promotion of the built tarball, and readback.
fn npm_steps(context: &RecipeContext<'_>) -> Result<String, String> {
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
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n      @ENVVAR@SUBJECT_IDENTITY: {}\n{}    run: |\n{}{}{}",
        scalar(&format!("Authenticate the {identity} publication")),
        scalar(registry),
        scalar(context.subject_identity),
        if primary {
            format!("      @ENVVAR@BOOTSTRAP_TOKEN: ${{{{ secrets.{bootstrap} }}}}\n")
        } else {
            "      @ENVVAR@GITHUB_PACKAGES_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n".to_owned()
        },
        STRICT_MODE,
        if primary { NPM_HOLDS } else { "" },
        if primary {
            NPM_TRUSTED_AUTHENTICATION
        } else {
            NPM_GITHUB_AUTHENTICATION
        },
    ));

    steps.push_str(&format!(
        "  - name: {}\n    working-directory: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n{}    run: |\n{}{}{}",
        scalar(&format!("Publish {identity}")),
        scalar(context.working_directory),
        scalar(registry),
        subject_environment(context),
        STRICT_MODE,
        NPM_HOLDS,
        if primary {
            NPM_PUBLISH_PRIMARY
        } else {
            NPM_PUBLISH_GITHUB
        },
    ));

    steps.push_str(&format!(
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n      @ENVVAR@DESTINATION: {}\n{}{}{}{}    run: |\n{}{}{}{}",
        scalar(&format!("Read {identity} back and retrieve it")),
        scalar(registry),
        scalar(destination),
        if primary {
            String::new()
        } else {
            "      @ENVVAR@GITHUB_PACKAGES_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n".to_owned()
        },
        subject_environment(context),
        observation_environment(context, "npm-package", "npm"),
        policy_environment(context.publication.publisher),
        STRICT_MODE,
        OBSERVE,
        NPM_HOLDS,
        npm_readback(if primary {
            NPM_RETRIEVE_PUBLIC
        } else {
            NPM_RETRIEVE_AUTHENTICATED
        }),
    ));
    Ok(steps)
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
const NPM_HOLDS: &str = r#"      @ENVVAR@npm_holds() {
        if @ENVVAR@VIEW="$(npm view "$1" dist.integrity --registry "${@ENVVAR@REGISTRY}" \
          2>"${RUNNER_TEMP}/@JOB@npm-error")"; then
          printf '%s' "${@ENVVAR@VIEW}"
          return 0
        fi
        case "$(cat "${RUNNER_TEMP}/@JOB@npm-error")" in
          *E404*|*"404 Not Found"*) return 1 ;;
          *) cat "${RUNNER_TEMP}/@JOB@npm-error" >&2 ; return 2 ;;
        esac
      }
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
    // A crates.io retrieval records the public consumer path, so the resolve
    // that performs it withholds the publish credential the authenticate step
    // exported. An alternate registry's retrieval is the authenticated one it
    // records, and needs the credential to read the index at all.
    let withheld = crates_io.then_some(token_variable.as_str());
    let registry_environment = format!(
        "      @ENVVAR@REGISTRY: {}\n      @ENVVAR@REGISTRY_NAME: {}\n",
        scalar(registry),
        scalar(&registry_name),
    );
    let mut steps = String::new();

    steps.push_str(&format!(
        "  - name: {}\n    env:\n{registry_environment}      @ENVVAR@BOOTSTRAP_TOKEN: ${{{{ secrets.{bootstrap} }}}}\n      @ENVVAR@TOKEN_VARIABLE: {}\n{}    run: |\n{}{}{}",
        scalar(&format!("Authenticate the {identity} publication")),
        scalar(&token_variable),
        subject_environment(context),
        STRICT_MODE,
        if crates_io {
            cargo_resolve(withheld)
        } else {
            String::new()
        },
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
        cargo_resolve(withheld),
        CARGO_PUBLISH,
    ));

    steps.push_str(&format!(
        "  - name: {}\n    env:\n{registry_environment}      @ENVVAR@DESTINATION: {}\n{}{}{}    run: |\n{}{}{}{}",
        scalar(&format!("Read {identity} back and retrieve it")),
        scalar(registry),
        subject_environment(context),
        observation_environment(context, "cargo-crate", "cargo"),
        policy_environment(context.publication.publisher),
        STRICT_MODE,
        OBSERVE,
        cargo_resolve(withheld),
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
/// The scratch crate copies the workspace's `.cargo/config.toml` when there is
/// one, because an alternate registry is declared there and a crate outside the
/// workspace would otherwise not know the name it is being asked to resolve.
///
/// The three outcomes are distinct. Cargo reports a crate the index does not
/// carry differently from a fetch it could not perform, and only the first is
/// evidence of absence; treating both as absence routes a transient failure
/// into the bootstrap credential.
///
/// Offline resolution is the one case where those two are spelled the same.
/// `cargo add` under `net.offline` reports a crate it did not look for with the
/// same words it uses for a crate the index does not carry, and the copied
/// workspace configuration can carry `[net] offline = true`, so a repository
/// could make every probe report absence for a crate the registry has held for
/// years. The probe forces the network on rather than trying to tell the two
/// messages apart, so the message that means "I did not look" cannot be
/// produced.
///
/// The resolve also drops the publish credential. It is exported into
/// `GITHUB_ENV` by the authenticate step and so is present in this process, and
/// crates.io records a public consumer retrieval: reading an index and
/// downloading a crate from crates.io does not present that token, but that is
/// a fact about cargo rather than a property of this step. Unsetting it makes
/// the recorded claim structural, the way the npm side's scratch configuration
/// does. A destination whose recipe records an authenticated retrieval keeps
/// its credential, because there the consumer path is the authenticated one.
fn cargo_resolve(withheld: Option<&str>) -> String {
    let withhold = withheld.map_or_else(String::new, |variable| format!("env -u {variable} "));
    format!(
        r#"      @ENVVAR@REGISTRY_ARGUMENTS=()
      if [ -n "${{@ENVVAR@REGISTRY_NAME:-}}" ]; then
        @ENVVAR@REGISTRY_ARGUMENTS=(--registry "${{@ENVVAR@REGISTRY_NAME}}")
      fi
      @ENVVAR@resolve() {{
        rm -rf "$1"
        mkdir -p "$1"
        cargo new --quiet --lib "$1/probe" >/dev/null
        mkdir -p "$1/probe/.cargo"
        if [ -f "${{GITHUB_WORKSPACE}}/.cargo/config.toml" ]; then
          cp "${{GITHUB_WORKSPACE}}/.cargo/config.toml" "$1/probe/.cargo/config.toml"
        fi
        if ( cd "$1/probe" \
          && {withhold}CARGO_NET_OFFLINE=false CARGO_HOME="$1/home" \
            cargo add --quiet "${{@ENVVAR@REGISTRY_ARGUMENTS[@]}}" \
            "${{@ENVVAR@SUBJECT_IDENTITY}}@=${{@ENVVAR@VERSION}}" \
          && {withhold}CARGO_NET_OFFLINE=false CARGO_HOME="$1/home" \
            cargo fetch --quiet ) > "$1/log" 2>&1; then
          return 0
        fi
        if grep -qiE 'could not be found|not found in registry|no matching package' "$1/log"; then
          return 1
        fi
        cat "$1/log" >&2
        return 2
      }}
"#
    )
}

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
