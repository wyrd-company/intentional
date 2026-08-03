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
//! Three rules shape every script here.
//!
//! Values arrive through `env:` and are read as shell variables. A `${{ }}`
//! expansion inside a `run:` body is textual substitution into shell source, and
//! these bodies hold publisher credentials.
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

use crate::config::ReleaseUnitConfig;
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

/// Lowest npm release that implements registry trusted publishing.
///
/// A stock runner's bundled npm is older than this on the images this executor
/// targets, and an older client falls back to looking for a token that a
/// trusted-publishing repository deliberately does not hold. Stating the
/// requirement as a range makes the failure a resolution error naming the
/// version rather than an authentication error naming nothing.
const NPM_TRUSTED_PUBLISHING_RANGE: &str = ">=11.5.1";

/// Steps one publication's maintained recipe contributes to its publisher job.
pub(super) fn recipe_steps(context: &RecipeContext<'_>) -> Result<String, String> {
    match context.publication.packager {
        Packager::Npm => npm_steps(context),
        Packager::Cargo => cargo_steps(context),
        // The GoReleaser, Buildx and Dev Container recipes still promote their
        // subject through one native command. Their authentication, readback and
        // retrieval belong to the tasks that own those destinations.
        Packager::GoReleaser | Packager::Buildx | Packager::DevContainerCli => {
            Ok(promote_only(context))
        }
    }
}

/// The single promotion step a recipe without derived readback still emits.
fn promote_only(context: &RecipeContext<'_>) -> String {
    format!(
        "  - name: {}\n    working-directory: {}\n    env:\n      @ENVVAR@SUBJECT: ${{{{ runner.temp }}}}/@JOB@subject/bytes\n    run: {}\n",
        scalar(&format!("Publish {}", context.publication.identity())),
        scalar(context.working_directory),
        package_command(context.publication.packager),
    )
}

/// Native command a recipe without derived readback drives.
const fn package_command(packager: Packager) -> &'static str {
    match packager {
        Packager::GoReleaser => "goreleaser release --clean",
        Packager::Buildx => "docker buildx build --push --provenance true --sbom true .",
        Packager::DevContainerCli => {
            "devcontainer features publish --namespace \"${GITHUB_REPOSITORY}\" ."
        }
        // Both derive their own steps, and this arm exists only because the
        // packager set is closed.
        Packager::Npm => "npm publish --provenance --access public",
        Packager::Cargo => "cargo publish --locked",
    }
}

/// Reject a configured secret name GitHub could not resolve.
///
/// The name is spliced into a `${{ secrets.NAME }}` expression, which is an
/// identifier position rather than a value position: a name carrying a bracket
/// or a quote would not name a missing secret, it would change what the
/// expression evaluates.
fn secret_name(configured: Option<&str>, conventional: &str) -> Result<String, String> {
    let Some(name) = configured else {
        return Ok(conventional.to_owned());
    };
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !name.starts_with(|character: char| character.is_ascii_digit());
    if valid {
        Ok(name.to_owned())
    } else {
        Err(format!(
            "token-secret {name:?} is not a GitHub secret name; a secret name contains letters, digits and underscores and does not start with a digit"
        ))
    }
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
        policy.interval.as_secs(),
        policy.backoff,
        policy.maximum_interval.as_secs(),
        policy.deadline.as_secs(),
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
fn observation_environment(context: &RecipeContext<'_>) -> String {
    format!(
        "      @ENVVAR@OBSERVATION: {}\n      @ENVVAR@RELEASE_UNIT: {}\n      @ENVVAR@PUBLISHER: {}\n      @ENVVAR@TARGET: {}\n      @ENVVAR@WORK: {}\n",
        scalar(context.observation),
        scalar(&context.publication.release_unit),
        scalar(context.publication.publisher.as_str()),
        scalar(&context.publication.target),
        scalar(context.work),
    )
}

/// Shell every recipe step opens with.
///
/// The strict-mode line comes before any helper definition so a reader can see
/// at a glance that everything below it is guarded, and so a helper added later
/// cannot quietly land above the line that makes a failure fatal.
const STRICT_MODE: &str = "      set -euo pipefail\n";

/// Shell writing one unobservable or disagreeing observation and succeeding.
///
/// A state carrying no destination detail is the same three lines for every
/// adapter, and the command that reads it is what turns `pending` into a
/// bounded wait and `conflict` into an immediate failure.
const OBSERVE_WITHOUT_DETAIL: &str = r#"      @ENVVAR@observe_state() {
        mkdir -p "$(dirname "${@ENVVAR@OBSERVATION}")"
        {
          printf '$schema: https://intentional.foo/schemas/publication-observation/v1\n'
          printf 'contract: publication-observation-1\n'
          printf 'release-unit: "%s"\n' "${@ENVVAR@RELEASE_UNIT}"
          printf 'publisher: "%s"\n' "${@ENVVAR@PUBLISHER}"
          printf 'target: "%s"\n' "${@ENVVAR@TARGET}"
          printf 'state: %s\n' "$1"
          if [ "$1" = conflict ]; then printf 'conflict: "%s"\n' "$2"; fi
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
    let bootstrap = secret_name(
        context
            .unit
            .npm
            .as_ref()
            .and_then(|npm| npm.token_secret.as_deref()),
        NPM_TOKEN_SECRET,
    )?;
    let mut steps = String::new();

    // Trusted publishing is an npm client capability rather than a registry
    // negotiation, so the client that performs it has to be new enough to know
    // the exchange exists. GitHub Package Registry does not implement it, and
    // its recipe stays on the runner's own client.
    if primary {
        steps.push_str(&format!(
            "  - name: Prepare the npm client for trusted publishing\n    env:\n      @ENVVAR@NPM_RANGE: {}\n    run: |\n      set -euo pipefail\n      npm install --global \"npm@${{@ENVVAR@NPM_RANGE}}\"\n      npm --version\n",
            scalar(NPM_TRUSTED_PUBLISHING_RANGE),
        ));
    }

    steps.push_str(&format!(
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n      @ENVVAR@SUBJECT_IDENTITY: {}\n{}    run: |\n{}{}",
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
            NPM_TRUSTED_AUTHENTICATION
        } else {
            NPM_GITHUB_AUTHENTICATION
        },
    ));

    steps.push_str(&format!(
        "  - name: {}\n    working-directory: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n{}    run: |\n{}{}",
        scalar(&format!("Publish {identity}")),
        scalar(context.working_directory),
        scalar(registry),
        subject_environment(context),
        STRICT_MODE,
        if primary {
            NPM_PUBLISH_PRIMARY
        } else {
            NPM_PUBLISH_GITHUB
        },
    ));

    steps.push_str(&format!(
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n      @ENVVAR@DESTINATION: {}\n      @ENVVAR@RETRIEVAL_MODE: {}\n{}{}{}    run: |\n{}{}{}",
        scalar(&format!("Read {identity} back and retrieve it")),
        scalar(registry),
        scalar(destination),
        if primary {
            "public"
        } else {
            "authenticated-registry"
        },
        subject_environment(context),
        observation_environment(context),
        policy_environment(context.publication.publisher),
        STRICT_MODE,
        OBSERVE_WITHOUT_DETAIL,
        NPM_READBACK,
    ));
    Ok(steps)
}

/// Trusted-publishing authentication with a protected first-publication path.
///
/// npmjs can only bind a trusted publisher to a package that already exists, so
/// the very first publication of a package has nothing to be trusted against.
/// The bootstrap token covers exactly that case and is reachable only while the
/// package is absent: once the destination holds the package, this step never
/// reads the secret again, so a trusted-identity failure cannot silently fall
/// back to a long-lived token.
const NPM_TRUSTED_AUTHENTICATION: &str = r#"      npm config set registry "${@ENVVAR@REGISTRY}"
      if npm view "${@ENVVAR@SUBJECT_IDENTITY}" version --registry "${@ENVVAR@REGISTRY}" >/dev/null 2>&1; then
        echo "${@ENVVAR@SUBJECT_IDENTITY} exists; this publication uses its configured trusted publisher"
        exit 0
      fi
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
/// decides whether what is there is this release.
const NPM_PUBLISH_PRIMARY: &str = r#"      @ENVVAR@TARBALL="$(find "${@ENVVAR@SUBJECT}" -maxdepth 1 -name '*.tgz' -print -quit)"
      test -n "${@ENVVAR@TARBALL}"
      if npm view "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" dist.integrity \
        --registry "${@ENVVAR@REGISTRY}" >/dev/null 2>&1; then
        echo "the destination already holds this version; the readback decides whether it is this release"
        exit 0
      fi
      npm publish "${@ENVVAR@TARBALL}" --provenance --access public \
        --registry "${@ENVVAR@REGISTRY}"
"#;

/// Promotion to GitHub Package Registry.
///
/// The registry generates no provenance attestation, so the recipe does not ask
/// for one it could not then record as an attached component.
const NPM_PUBLISH_GITHUB: &str = r#"      @ENVVAR@TARBALL="$(find "${@ENVVAR@SUBJECT}" -maxdepth 1 -name '*.tgz' -print -quit)"
      test -n "${@ENVVAR@TARBALL}"
      if npm view "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" dist.integrity \
        --registry "${@ENVVAR@REGISTRY}" >/dev/null 2>&1; then
        echo "the destination already holds this version; the readback decides whether it is this release"
        exit 0
      fi
      npm publish "${@ENVVAR@TARBALL}" --registry "${@ENVVAR@REGISTRY}"
"#;

/// Destination readback, clean-client retrieval, and the observation they produce.
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
      @ENVVAR@INDEXED=""
      @ENVVAR@ELAPSED=0
      while : ; do
        @ENVVAR@INDEXED="$(npm view "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" dist.integrity \
          --registry "${@ENVVAR@REGISTRY}" 2>/dev/null || true)"
        if [ -n "${@ENVVAR@INDEXED}" ]; then break; fi
        if [ "${@ENVVAR@ELAPSED}" -ge "${@ENVVAR@DEADLINE}" ]; then break; fi
        sleep "${@ENVVAR@INTERVAL}"
        @ENVVAR@ELAPSED=$(( @ENVVAR@ELAPSED + @ENVVAR@INTERVAL ))
        @ENVVAR@INTERVAL=$(( @ENVVAR@INTERVAL * @ENVVAR@BACKOFF ))
        if [ "${@ENVVAR@INTERVAL}" -gt "${@ENVVAR@MAXIMUM_INTERVAL}" ]; then
          @ENVVAR@INTERVAL="${@ENVVAR@MAXIMUM_INTERVAL}"
        fi
      done
      if [ -z "${@ENVVAR@INDEXED}" ]; then
        @ENVVAR@observe_state pending
        exit 0
      fi
      if [ "${@ENVVAR@INDEXED}" != "${@ENVVAR@LOCAL}" ]; then
        @ENVVAR@observe_state conflict \
          "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION} publishes integrity ${@ENVVAR@INDEXED}, not the promoted ${@ENVVAR@LOCAL}"
        exit 0
      fi
      rm -rf "${@ENVVAR@WORK}/clean"
      mkdir -p "${@ENVVAR@WORK}/clean"
      ( cd "${@ENVVAR@WORK}/clean" && npm pack "${@ENVVAR@SUBJECT_IDENTITY}@${@ENVVAR@VERSION}" \
        --registry "${@ENVVAR@REGISTRY}" --cache "${@ENVVAR@WORK}/clean/cache" >/dev/null )
      @ENVVAR@RETRIEVED="$(find "${@ENVVAR@WORK}/clean" -maxdepth 1 -name '*.tgz' -print -quit)"
      test -n "${@ENVVAR@RETRIEVED}"
      @ENVVAR@RETRIEVED_INTEGRITY="sha512-$(openssl dgst -sha512 -binary "${@ENVVAR@RETRIEVED}" | base64 -w0)"
      test "${@ENVVAR@RETRIEVED_INTEGRITY}" = "${@ENVVAR@LOCAL}"
      mkdir -p "$(dirname "${@ENVVAR@OBSERVATION}")"
      {
        printf '$schema: https://intentional.foo/schemas/publication-observation/v1\n'
        printf 'contract: publication-observation-1\n'
        printf 'release-unit: "%s"\n' "${@ENVVAR@RELEASE_UNIT}"
        printf 'publisher: "%s"\n' "${@ENVVAR@PUBLISHER}"
        printf 'target: "%s"\n' "${@ENVVAR@TARGET}"
        printf 'state: present\n'
        printf 'subject:\n'
        printf '  kind: npm-package\n'
        printf '  identity: "%s"\n' "${@ENVVAR@SUBJECT_IDENTITY}"
        printf '  version: "%s"\n' "${@ENVVAR@VERSION}"
        printf '  digest: "%s"\n' "${@ENVVAR@SUBJECT_DIGEST}"
        printf 'packager:\n'
        printf '  id: npm\n'
        printf '  version: "%s"\n' "$(npm --version)"
        printf 'destination:\n'
        printf '  identity: "%s"\n' "${@ENVVAR@DESTINATION}"
        printf '  version: "%s"\n' "${@ENVVAR@VERSION}"
        printf '  digest: "%s"\n' "${@ENVVAR@INDEXED}"
        printf 'retrieval:\n'
        printf '  mode: %s\n' "${@ENVVAR@RETRIEVAL_MODE}"
        printf '  client: npm\n'
        printf '  version: "%s"\n' "$(npm --version)"
        printf '  digest: "%s"\n' "${@ENVVAR@RETRIEVED_INTEGRITY}"
      } > "${@ENVVAR@OBSERVATION}"
"#;

/// Cargo recipe: trusted publishing, a promotion gate, and cargo's own retrieval.
fn cargo_steps(context: &RecipeContext<'_>) -> Result<String, String> {
    let identity = context.publication.identity();
    let registry = context
        .publication
        .destination
        .as_deref()
        .unwrap_or(CRATES_IO);
    let bootstrap = secret_name(
        context
            .unit
            .cargo
            .as_ref()
            .and_then(|cargo| cargo.token_secret.as_deref()),
        CARGO_TOKEN_SECRET,
    )?;
    // Cargo names an alternate registry on the command line and reads its
    // credential from the matching environment variable, so the flag and the
    // variable are derived together from the one configured destination.
    let (flag, token_variable) = if registry == CRATES_IO {
        (String::new(), "CARGO_REGISTRY_TOKEN".to_owned())
    } else {
        (
            format!(" --registry {registry}"),
            format!("CARGO_REGISTRIES_{}_TOKEN", environment_fragment(registry)),
        )
    };
    let mut steps = String::new();

    steps.push_str(&format!(
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n      @ENVVAR@BOOTSTRAP_TOKEN: ${{{{ secrets.{bootstrap} }}}}\n      @ENVVAR@TOKEN_VARIABLE: {}\n{}    run: |\n{}{}{}",
        scalar(&format!("Authenticate the {identity} publication")),
        scalar(registry),
        scalar(&token_variable),
        subject_environment(context),
        STRICT_MODE,
        if registry == CRATES_IO {
            cargo_probe(&flag)
        } else {
            String::new()
        },
        if registry == CRATES_IO {
            CARGO_TRUSTED_AUTHENTICATION
        } else {
            CARGO_TOKEN_AUTHENTICATION
        },
    ));

    steps.push_str(&format!(
        "  - name: {}\n    working-directory: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n{}    run: |\n{}{}{}",
        scalar(&format!("Publish {identity}")),
        scalar(context.working_directory),
        scalar(registry),
        subject_environment(context),
        STRICT_MODE,
        cargo_probe(&flag),
        cargo_publish(&flag),
    ));

    steps.push_str(&format!(
        "  - name: {}\n    env:\n      @ENVVAR@REGISTRY: {}\n{}{}{}    run: |\n{}{}{}{}",
        scalar(&format!("Read {identity} back and retrieve it")),
        scalar(registry),
        subject_environment(context),
        observation_environment(context),
        policy_environment(context.publication.publisher),
        STRICT_MODE,
        OBSERVE_WITHOUT_DETAIL,
        cargo_probe(&flag),
        CARGO_READBACK,
    ));
    Ok(steps)
}

/// Default Cargo registry, which is also the one that implements trusted publishing.
const CRATES_IO: &str = "crates.io";

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
fn cargo_probe(flag: &str) -> String {
    format!(
        r#"      @ENVVAR@resolve() {{
        rm -rf "$1"
        mkdir -p "$1"
        cargo new --quiet --lib "$1/probe" >/dev/null
        mkdir -p "$1/probe/.cargo"
        if [ -f "${{GITHUB_WORKSPACE}}/.cargo/config.toml" ]; then
          cp "${{GITHUB_WORKSPACE}}/.cargo/config.toml" "$1/probe/.cargo/config.toml"
        fi
        ( cd "$1/probe" \
          && CARGO_HOME="$1/home" cargo add --quiet{flag} \
            "${{@ENVVAR@SUBJECT_IDENTITY}}@=${{@ENVVAR@VERSION}}" >/dev/null 2>&1 \
          && CARGO_HOME="$1/home" cargo fetch --quiet >/dev/null 2>&1 )
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
/// covers; once the crate resolves, this step stops reading the secret.
const CARGO_TRUSTED_AUTHENTICATION: &str = r#"      if ! @ENVVAR@resolve "${RUNNER_TEMP}/@JOB@bootstrap-probe"; then
        if [ -z "${@ENVVAR@BOOTSTRAP_TOKEN:-}" ]; then
          echo "${@ENVVAR@SUBJECT_IDENTITY} does not exist yet and no bootstrap token secret is available" >&2
          echo "a trusted publisher can only be configured for an existing crate" >&2
          exit 1
        fi
        echo "${@ENVVAR@TOKEN_VARIABLE}=${@ENVVAR@BOOTSTRAP_TOKEN}" >> "${GITHUB_ENV}"
        exit 0
      fi
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
/// necessarily re-packages the release-unit sources. That is a rebuild, and a
/// rebuild that drifted would send the registry bytes no phase tag sealed, so
/// the recipe proves the packaging is reproducible before it uploads anything:
/// it re-packages here and refuses to publish unless the result is byte-identical
/// to the subject the build job produced. Discovering the drift afterwards would
/// be too late, because a published crate version is immutable.
fn cargo_publish(flag: &str) -> String {
    format!(
        r#"      @ENVVAR@CRATE="$(find "${{@ENVVAR@SUBJECT}}" -maxdepth 1 -name '*.crate' -print -quit)"
      test -n "${{@ENVVAR@CRATE}}"
      if @ENVVAR@resolve "${{RUNNER_TEMP}}/@JOB@publish-probe"; then
        echo "the destination already holds this version; the readback decides whether it is this release"
        exit 0
      fi
      cargo package --locked --no-verify --target-dir "${{RUNNER_TEMP}}/@JOB@repackage"
      @ENVVAR@REPACKAGED="${{RUNNER_TEMP}}/@JOB@repackage/package/$(basename "${{@ENVVAR@CRATE}}")"
      test "$(sha256sum < "${{@ENVVAR@REPACKAGED}}" | cut -d' ' -f1)" \
        = "$(sha256sum < "${{@ENVVAR@CRATE}}" | cut -d' ' -f1)"
      cargo publish --locked --no-verify{flag}
"#
    )
}

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
      @ENVVAR@PUBLISHED="$(sed -n "/^name = \"${@ENVVAR@SUBJECT_IDENTITY}\"$/,/^$/p" \
        "${@ENVVAR@WORK}/clean/probe/Cargo.lock" | sed -n 's/^checksum = "\(.*\)"$/\1/p')"
      test -n "${@ENVVAR@PUBLISHED}"
      @ENVVAR@RETRIEVED="$(find "${@ENVVAR@WORK}/clean/home/registry/cache" -type f \
        -name "${@ENVVAR@SUBJECT_IDENTITY}-${@ENVVAR@VERSION}.crate" -print -quit)"
      test -n "${@ENVVAR@RETRIEVED}"
      @ENVVAR@RETRIEVED_DIGEST="$(sha256sum < "${@ENVVAR@RETRIEVED}" | cut -d' ' -f1)"
      if [ "${@ENVVAR@PUBLISHED}" != "${@ENVVAR@LOCAL}" ]; then
        @ENVVAR@observe_state conflict \
          "${@ENVVAR@SUBJECT_IDENTITY} ${@ENVVAR@VERSION} publishes checksum ${@ENVVAR@PUBLISHED}, not the promoted ${@ENVVAR@LOCAL}"
        exit 0
      fi
      test "${@ENVVAR@RETRIEVED_DIGEST}" = "${@ENVVAR@LOCAL}"
      mkdir -p "$(dirname "${@ENVVAR@OBSERVATION}")"
      {
        printf '$schema: https://intentional.foo/schemas/publication-observation/v1\n'
        printf 'contract: publication-observation-1\n'
        printf 'release-unit: "%s"\n' "${@ENVVAR@RELEASE_UNIT}"
        printf 'publisher: "%s"\n' "${@ENVVAR@PUBLISHER}"
        printf 'target: "%s"\n' "${@ENVVAR@TARGET}"
        printf 'state: present\n'
        printf 'subject:\n'
        printf '  kind: cargo-crate\n'
        printf '  identity: "%s"\n' "${@ENVVAR@SUBJECT_IDENTITY}"
        printf '  version: "%s"\n' "${@ENVVAR@VERSION}"
        printf '  digest: "%s"\n' "${@ENVVAR@SUBJECT_DIGEST}"
        printf 'packager:\n'
        printf '  id: cargo\n'
        printf '  version: "%s"\n' "$(cargo --version | cut -d' ' -f2)"
        printf 'destination:\n'
        printf '  identity: "%s"\n' "${@ENVVAR@REGISTRY}"
        printf '  version: "%s"\n' "${@ENVVAR@VERSION}"
        printf '  digest: "%s"\n' "${@ENVVAR@PUBLISHED}"
        printf 'retrieval:\n'
        printf '  mode: public\n'
        printf '  client: cargo\n'
        printf '  version: "%s"\n' "$(cargo --version | cut -d' ' -f2)"
        printf '  digest: "%s"\n' "${@ENVVAR@RETRIEVED_DIGEST}"
      } > "${@ENVVAR@OBSERVATION}"
"#;
