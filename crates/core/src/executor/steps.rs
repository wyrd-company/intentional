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

use crate::config::{AptPublisher, ReleaseUnitConfig, RpmPublisher};
use crate::executor::goreleaser::nfpm_format;
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
    /// Workspace-relative directory that owns the packager invocation.
    pub working_directory: &'a str,
    /// Observation path shared by the recipe writer and portable verifier.
    pub observation: &'a str,
    /// Scratch directory the readback and retrieval work in.
    pub work: &'a str,
    /// Configured job prefix converted to kebab case for delivery inputs.
    pub delivery_namespace: &'a str,
    /// Workspace root, read for the native configuration a probe must not inherit.
    pub root: &'a std::path::Path,
}

/// Repository-local steps divided by the authority their job requires.
pub(super) struct RecipeSteps {
    /// Steps that authenticate and publish, and normally also retrieve.
    pub publisher: String,
    /// Consumer retrieval isolated from publish authority when required.
    pub retrieval: Option<String>,
    /// Inputs the portable verification Action needs to perform readback.
    pub observation_inputs: String,
    /// Whether repository-visible shell has already written the observation.
    pub observation_inline: bool,
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
/// registry. GoReleaser and the Cargo archive route produce them: an npm tarball, a `.crate`, an
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
        Packager::CargoArchive => Some(""),
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

/// npm release used for registry trusted publishing.
///
/// A stock runner's bundled npm is older than this on the images this executor
/// targets, and an older client falls back to looking for a token that a
/// trusted-publishing repository deliberately does not hold. The exact version
/// keeps the client executing with publication authority inside the same
/// reviewed supply-chain boundary as the executor's other dependencies.
const NPM_TRUSTED_PUBLISHING_VERSION: &str = "11.5.1";

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
        observation_inputs: steps.observation_inputs,
        observation_inline: steps.observation_inline,
    })
}

/// Whether workflow derivation has a complete publisher recipe for this pair.
pub(super) const fn recipe_is_derived(packager: Packager, publisher: PublisherKind) -> bool {
    matches!(
        (packager, publisher),
        (Packager::Npm, PublisherKind::Npm)
            | (Packager::Cargo, PublisherKind::Cargo)
            | (
                Packager::GoReleaser,
                PublisherKind::Homebrew
                    | PublisherKind::Aur
                    | PublisherKind::Rpm
                    | PublisherKind::Apt
            )
            | (
                Packager::CargoArchive,
                PublisherKind::Homebrew
                    | PublisherKind::Aur
                    | PublisherKind::Rpm
                    | PublisherKind::Apt
            )
            | (Packager::Buildx, PublisherKind::Oci)
            | (Packager::DevContainerCli, PublisherKind::Oci)
    )
}

/// One publication's recipe steps before the shared placeholders are rendered.
fn steps_for(context: &RecipeContext<'_>) -> Result<RecipeSteps, StepsRefusal> {
    let identity = context.publication.identity();
    let underivable = |message: String| StepsRefusal::underivable(&identity, &message);
    match context.publication.packager {
        Packager::Npm => npm_steps(context).map_err(underivable),
        Packager::Cargo => cargo_steps(context).map_err(underivable),
        Packager::CargoArchive | Packager::GoReleaser => descriptor_promotion_steps(context),
        Packager::Buildx | Packager::DevContainerCli => oci_steps(context),
    }
}

/// Inputs shared by every portable publication observer.
fn portable_observation_inputs(
    context: &RecipeContext<'_>,
    kind: &str,
    packager: &str,
    client: &str,
    destination: &str,
) -> String {
    let maintained = ConsistencyPolicy::maintained(context.publication.publisher);
    let policy = context
        .publication
        .observation_deadline
        .map_or(maintained, |seconds| maintained.with_deadline(seconds));
    format!(
        "      subject: ${{{{ runner.temp }}}}/@JOB@subject/bytes\n      subject-kind: {}\n      subject-identity: {}\n      subject-version: ${{{{ needs.{}.outputs.version }}}}\n      subject-digest: ${{{{ needs.{}.outputs.digest }}}}\n      packager: {}\n      destination: {}\n      retrieval-mode: {}\n      retrieval-client: {}\n      work: {}\n      interval: {}\n      backoff: {}\n      maximum-interval: {}\n      deadline: {}\n",
        scalar(kind),
        scalar(context.subject_identity),
        context.build_job,
        context.build_job,
        scalar(packager),
        scalar(destination),
        scalar(context.publication.retrieval.as_str()),
        scalar(client),
        scalar(context.work),
        scalar(&policy.interval.as_secs().to_string()),
        scalar(&policy.backoff.to_string()),
        scalar(&policy.maximum_interval.as_secs().to_string()),
        scalar(&policy.deadline.as_secs().to_string()),
    )
}

const OBSERVER_COMMON: &str =
    include_str!("../../../../scripts/action/observe-publication/common.sh");
const NPM_OBSERVER: &str = include_str!("../../../../scripts/action/observe-publication/npm.sh");
const CARGO_OBSERVER: &str =
    include_str!("../../../../scripts/action/observe-publication/cargo.sh");

fn indent_observer(source: &str) -> String {
    source
        .lines()
        .map(|line| format!("      {line}\n"))
        .collect()
}

/// Emit an authenticated observer inline without handing its credential to an Action.
fn inline_observation_step(
    context: &RecipeContext<'_>,
    kind: &str,
    packager: &str,
    client: &str,
    destination: &str,
    adapter: &str,
    adapter_environment: &str,
) -> String {
    let maintained = ConsistencyPolicy::maintained(context.publication.publisher);
    let policy = context
        .publication
        .observation_deadline
        .map_or(maintained, |seconds| maintained.with_deadline(seconds));
    format!(
        "  - name: {}\n    env:\n      INPUT_RELEASE_UNIT: {}\n      INPUT_PACKAGE: {}\n      INPUT_PUBLISHER: {}\n      INPUT_TARGET: {}\n      INPUT_OBSERVATION: {}\n      INPUT_SUBJECT: ${{{{ runner.temp }}}}/@JOB@subject/bytes\n      INPUT_SUBJECT_KIND: {}\n      INPUT_SUBJECT_IDENTITY: {}\n      INPUT_SUBJECT_VERSION: ${{{{ needs.{}.outputs.version }}}}\n      INPUT_SUBJECT_DIGEST: ${{{{ needs.{}.outputs.digest }}}}\n      INPUT_PACKAGER: {}\n      INPUT_DESTINATION: {}\n      INPUT_RETRIEVAL_MODE: {}\n      INPUT_RETRIEVAL_CLIENT: {}\n      INPUT_WORK: {}\n      INPUT_INTERVAL: {}\n      INPUT_BACKOFF: {}\n      INPUT_MAXIMUM_INTERVAL: {}\n      INPUT_DEADLINE: {}\n{adapter_environment}    run: |\n{}{}",
        scalar(&format!("Read {} back and retrieve it", context.publication.identity())),
        scalar(&context.publication.release_unit),
        scalar(&context.publication.package),
        scalar(context.publication.publisher.as_str()),
        scalar(&context.publication.target),
        scalar(context.observation),
        scalar(kind),
        scalar(context.subject_identity),
        context.build_job,
        context.build_job,
        scalar(packager),
        scalar(destination),
        scalar(context.publication.retrieval.as_str()),
        scalar(client),
        scalar(context.work),
        scalar(&policy.interval.as_secs().to_string()),
        scalar(&policy.backoff.to_string()),
        scalar(&policy.maximum_interval.as_secs().to_string()),
        scalar(&policy.deadline.as_secs().to_string()),
        indent_observer(OBSERVER_COMMON),
        indent_observer(adapter),
    )
}

/// Promote a sealed descriptor into its configured repository destination.
///
/// GoReleaser and Cargo archive builds seal their repository descriptors before
/// publication. Promotion copies those files into the configured destination;
/// it does not invoke either packager and cannot change the sealed subject.
fn descriptor_promotion_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, StepsRefusal> {
    let identity = context.publication.identity();
    // RPM and APT distribute the deliverable itself rather than a descriptor
    // that points at one, so the managed upload job places it on the draft
    if !recipe_is_derived(context.publication.packager, context.publication.publisher) {
        return Err(underived_recipe_refusal(
            &identity,
            context.publication.publisher,
        ));
    }
    if matches!(
        context.publication.publisher,
        PublisherKind::Rpm | PublisherKind::Apt
    ) {
        return system_package_steps(context);
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
    let kind = match context.publication.publisher {
        PublisherKind::Homebrew => "homebrew-formula",
        PublisherKind::Aur => "aur-package",
        publisher => unreachable!("{publisher} is refused above"),
    };
    let publisher = format!(
        "{credential}  - name: {}\n    env:\n{}      @ENVVAR@GLOBAL_TAG: ${{{{ github.ref_name }}}}\n{environment}    run: |\n      set -euo pipefail\n{command}\n",
        scalar(&format!("Publish {identity}")),
        subject_environment(context),
    );
    let observation_inputs = portable_observation_inputs(
        context,
        kind,
        context.publication.packager.as_str(),
        "git",
        &destination,
    );
    Ok(RecipeSteps {
        publisher,
        retrieval: (context.publication.publisher == PublisherKind::Homebrew).then(String::new),
        observation_inputs,
        observation_inline: false,
    })
}

pub(super) fn underived_recipe_refusal(identity: &str, publisher: PublisherKind) -> StepsRefusal {
    StepsRefusal {
        code: "maintained-recipe-underived",
        message: format!(
            "publication {identity} has no maintained {publisher} recipe derived for its publisher job"
        ),
        path: None,
    }
}

struct DeliveryConfiguration<'a> {
    action: &'a std::path::Path,
    base_url: &'a str,
    public_key_url: &'a str,
    inputs: &'a std::collections::BTreeMap<String, String>,
    coordinates: Vec<(&'static str, &'a str)>,
}

fn reserved_input(namespace: &str, stem: &str) -> String {
    format!("{namespace}-{stem}")
}

fn delivery_input_names(context: &RecipeContext<'_>, publisher: PublisherKind) -> Vec<String> {
    let mut stems = vec![
        "package-path",
        "format",
        "name",
        "version",
        "architecture",
        "digest",
    ];
    match publisher {
        PublisherKind::Apt => stems.extend(["apt-suite", "apt-component"]),
        PublisherKind::Rpm => stems.push("rpm-channel"),
        _ => unreachable!(),
    }
    stems
        .into_iter()
        .map(|stem| reserved_input(context.delivery_namespace, stem))
        .collect()
}

fn system_package_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, StepsRefusal> {
    let package = &context.unit.packages[&context.publication.package];
    let configured = match context.publication.publisher {
        PublisherKind::Rpm => {
            let RpmPublisher {
                delivery_action,
                base_url,
                public_signing_key_url,
                channel,
                inputs,
                ..
            } = package.rpm.as_ref().expect("selected rpm configuration");
            DeliveryConfiguration {
                action: delivery_action,
                base_url,
                public_key_url: public_signing_key_url,
                inputs,
                coordinates: vec![("rpm-channel", channel)],
            }
        }
        PublisherKind::Apt => {
            let AptPublisher {
                delivery_action,
                base_url,
                public_signing_key_url,
                suite,
                component,
                inputs,
                ..
            } = package.apt.as_ref().expect("selected apt configuration");
            DeliveryConfiguration {
                action: delivery_action,
                base_url,
                public_key_url: public_signing_key_url,
                inputs,
                coordinates: vec![("apt-suite", suite), ("apt-component", component)],
            }
        }
        _ => unreachable!(),
    };
    let reserved = delivery_input_names(context, context.publication.publisher);
    let declared = validate_delivery_action(context, &configured, &reserved)?;
    let supplied = reserved
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    for (name, definition) in &declared {
        let required = definition
            .get("required")
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false);
        let defaulted = definition.get("default").is_some();
        if required
            && !defaulted
            && !supplied.contains(name.as_str())
            && !configured.inputs.contains_key(name)
        {
            return Err(action_refusal(context, &configured, format!("delivery Action requires input {name:?} without a default, but neither the recipe nor its configured with block supplies it")));
        }
    }
    let mut with = String::new();
    for (stem, value) in [
        (
            "package-path",
            "${{ steps.intentional_establish.outputs.path }}".to_owned(),
        ),
        (
            "format",
            nfpm_format(context.publication.publisher)
                .expect("system package publisher has an nfpm format")
                .to_owned(),
        ),
        (
            "name",
            "${{ steps.intentional_establish.outputs.name }}".to_owned(),
        ),
        (
            "version",
            "${{ steps.intentional_establish.outputs.version }}".to_owned(),
        ),
        (
            "architecture",
            "${{ steps.intentional_establish.outputs.architecture }}".to_owned(),
        ),
        (
            "digest",
            "${{ steps.intentional_establish.outputs.digest }}".to_owned(),
        ),
    ] {
        let name = reserved_input(context.delivery_namespace, stem);
        with.push_str(&format!("      {name}: {}\n", scalar(&value)));
    }
    for (stem, value) in &configured.coordinates {
        let name = reserved_input(context.delivery_namespace, stem);
        with.push_str(&format!("      {name}: {}\n", scalar(value)));
    }
    for (name, value) in configured.inputs {
        with.push_str(&format!("      {}: {}\n", scalar(name), scalar(value)));
    }
    let format = context.publication.publisher.as_str();
    let metadata = if context.publication.publisher == PublisherKind::Apt {
        "name=$(dpkg-deb -f \"${package}\" Package)\n      version=$(dpkg-deb -f \"${package}\" Version)\n      architecture=$(dpkg-deb -f \"${package}\" Architecture)"
    } else {
        "name=$(rpm -qp --qf '%{NAME}' \"${package}\")\n      version=$(rpm -qp --qf '%{VERSION}' \"${package}\")\n      architecture=$(rpm -qp --qf '%{ARCH}' \"${package}\")"
    };
    let publisher = format!(
        "  - id: intentional_establish\n    name: {}\n    env:\n{}    run: |\n      set -euo pipefail\n      mapfile -t packages < <(find \"${{@ENVVAR@SUBJECT}}\" -maxdepth 1 -type f -print)\n      test \"${{#packages[@]}}\" -eq 1\n      package=${{packages[0]}}\n      digest=sha256:$(sha256sum \"${{package}}\" | cut -d' ' -f1)\n      test \"${{digest}}\" = \"${{@ENVVAR@SUBJECT_DIGEST}}\"\n      {metadata}\n      test \"${{name}}\" = \"${{@ENVVAR@SUBJECT_IDENTITY}}\"\n      test \"${{version}}\" = \"${{@ENVVAR@VERSION}}\"\n      printf 'path=%s\\nname=%s\\nversion=%s\\narchitecture=%s\\ndigest=%s\\n' \"${{package}}\" \"${{name}}\" \"${{version}}\" \"${{architecture}}\" \"${{digest}}\" >> \"${{GITHUB_OUTPUT}}\"\n  - name: {}\n    uses: {}\n    with:\n{with}",
        scalar(&format!("Establish the {} package", format.to_uppercase())),
        subject_environment(context),
        scalar(&format!("Deliver {}", context.publication.identity())),
        scalar(&local_action_uses(configured.action)?),
    );
    let client = if context.publication.publisher == PublisherKind::Apt {
        "apt"
    } else {
        "dnf"
    };
    let mut observation_inputs = portable_observation_inputs(
        context,
        "package",
        "goreleaser",
        client,
        configured.base_url,
    );
    observation_inputs.push_str(&format!(
        "      public-key-url: {}\n",
        scalar(configured.public_key_url)
    ));
    for (name, value) in configured.coordinates {
        observation_inputs.push_str(&format!("      {name}: {}\n", scalar(value)));
    }
    Ok(RecipeSteps {
        publisher,
        retrieval: None,
        observation_inputs,
        observation_inline: false,
    })
}

fn local_action_uses(action: &std::path::Path) -> Result<String, StepsRefusal> {
    if action.is_absolute()
        || action
            .extension()
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        || action.components().any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(_)
                    | std::path::Component::ParentDir
                    | std::path::Component::CurDir
            )
        })
    {
        return Err(StepsRefusal {
            code: "delivery-action-invalid",
            message: format!(
                "delivery Action path {} must name a workspace-relative directory",
                action.display()
            ),
            path: Some(action.display().to_string()),
        });
    }
    Ok(format!("./{}", action.display()))
}

fn action_refusal(
    context: &RecipeContext<'_>,
    configured: &DeliveryConfiguration<'_>,
    message: String,
) -> StepsRefusal {
    StepsRefusal {
        code: "delivery-action-invalid",
        message: format!(
            "publication {} names {}: {message}",
            context.publication.identity(),
            configured.action.display()
        ),
        path: Some(configured.action.display().to_string()),
    }
}

fn validate_delivery_action(
    context: &RecipeContext<'_>,
    configured: &DeliveryConfiguration<'_>,
    reserved: &[String],
) -> Result<std::collections::BTreeMap<String, serde_yaml::Value>, StepsRefusal> {
    local_action_uses(configured.action)?;
    let context_identity = format!("{} delivery Action", context.publication.publisher);
    let directory = context.root.join(configured.action);
    let candidates = [directory.join("action.yml"), directory.join("action.yaml")];
    let found = candidates
        .iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    let [metadata] = found.as_slice() else {
        return Err(StepsRefusal { code: "delivery-action-invalid", message: format!("{context_identity} directory {} must contain exactly one of action.yml or action.yaml", configured.action.display()), path: Some(configured.action.display().to_string()) });
    };
    let text = std::fs::read_to_string(metadata).map_err(|error| StepsRefusal {
        code: "delivery-action-invalid",
        message: format!("cannot read {}: {error}", metadata.display()),
        path: Some(metadata.display().to_string()),
    })?;
    let document: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|error| StepsRefusal {
            code: "delivery-action-invalid",
            message: format!(
                "{} is not valid Action metadata: {error}",
                metadata.display()
            ),
            path: Some(metadata.display().to_string()),
        })?;
    if document["runs"]["using"].as_str() != Some("composite") {
        return Err(StepsRefusal {
            code: "delivery-action-invalid",
            message: format!(
                "{} does not declare runs.using: composite",
                metadata.display()
            ),
            path: Some(metadata.display().to_string()),
        });
    }
    let inputs = document["inputs"]
        .as_mapping()
        .ok_or_else(|| StepsRefusal {
            code: "delivery-action-invalid",
            message: format!("{} declares no inputs mapping", metadata.display()),
            path: Some(metadata.display().to_string()),
        })?;
    let declared = inputs
        .iter()
        .filter_map(|(name, value)| name.as_str().map(|name| (name.to_owned(), value.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    for name in reserved {
        if !declared.contains_key(name) {
            return Err(StepsRefusal {
                code: "delivery-action-invalid",
                message: format!(
                    "{} does not declare reserved input {name}",
                    metadata.display()
                ),
                path: Some(metadata.display().to_string()),
            });
        }
    }
    for name in configured.inputs.keys() {
        let prefix = format!("{}-", context.delivery_namespace);
        if name.starts_with(&prefix) {
            return Err(StepsRefusal {
                code: "delivery-action-invalid",
                message: format!("configured input {name:?} is inside reserved namespace {prefix}"),
                path: Some(configured.action.display().to_string()),
            });
        }
        if !declared.contains_key(name) {
            return Err(StepsRefusal {
                code: "delivery-action-invalid",
                message: format!(
                    "{} does not declare configured input {name:?}",
                    metadata.display()
                ),
                path: Some(metadata.display().to_string()),
            });
        }
    }
    Ok(declared)
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
/// GoReleaser writes each declared `brews` formula beneath `homebrew`, while
/// Intentional writes each Cargo archive formula beneath `homebrew/Formula`.
/// The publisher promotes either source to the same relative path in the tap.
/// Promotion therefore preserves the path rather than
/// choosing one: a tap whose formulas do not live under `Formula` is stating
/// where they live, and this recipe's premise is that the native configuration
/// is the authority on that.
///
/// Every generated formula is promoted. A release unit with two `brews` entries
/// publishes both, and scoping the search to the packager's `homebrew` output
/// keeps a cask or any other generated Ruby file from being promoted as one.
///
/// A rerun that finds the tap already carrying this release commits nothing and
/// still succeeds. The fresh clone that follows decides whether the publication
/// is present rather than whether this step wrote.
const HOMEBREW_PROMOTE_COMMAND: &str = r#"      generated="${@ENVVAR@SUBJECT}/homebrew"
      test -d "${generated}"
      formulas=()
      while IFS= read -r -d '' formula; do
        formulas+=("${formula}")
      done < <(find "${generated}" -type f -name '*.rb' -print0 | sort -z)
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
        git init --quiet "${RUNNER_TEMP}/@JOB@aur"
        git -C "${RUNNER_TEMP}/@JOB@aur" symbolic-ref HEAD refs/heads/master
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

/// npm recipe: trusted publishing, promotion of the built tarball, and readback.
fn npm_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, String> {
    let primary = context.publication.target == PRIMARY_TARGET;
    let identity = context.publication.identity();
    let registry = if primary {
        NPMJS_REGISTRY
    } else {
        GITHUB_PACKAGES_REGISTRY
    };
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
            .npm()
            .and_then(|npm| npm.npmjs.as_ref())
            .and_then(|npmjs| npmjs.token_secret.as_deref())
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
            "  - name: Prepare the npm client for trusted publishing\n    env:\n      @ENVVAR@NPM_VERSION: {}\n    run: |\n{}      npm install --global \"npm@${{@ENVVAR@NPM_VERSION}}\"\n      npm --version\n",
            scalar(NPM_TRUSTED_PUBLISHING_VERSION),
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

    let observed_destination = context
        .publication
        .destination
        .as_deref()
        .unwrap_or(if primary {
            "npmjs"
        } else {
            GITHUB_PACKAGES_DESTINATION
        });
    let mut observation_inputs =
        portable_observation_inputs(context, "npm-package", "npm", "npm", observed_destination);
    observation_inputs.push_str(&format!(
        "      registry: {}\n      scope: {}\n",
        scalar(registry),
        scalar(scope),
    ));
    let retrieval = if primary {
        String::new()
    } else {
        observation_inputs.push_str("      observe: 'false'\n");
        inline_observation_step(
            context,
            "npm-package",
            "npm",
            "npm",
            observed_destination,
            NPM_OBSERVER,
            &format!(
                "      INPUT_REGISTRY: {}\n      INPUT_SCOPE: {}\n      INPUT_REGISTRY_TOKEN: ${{{{ secrets.GITHUB_TOKEN }}}}\n",
                scalar(registry),
                scalar(scope),
            ),
        )
    };
    Ok(RecipeSteps {
        publisher: steps,
        // Every observer is downstream from publication. GitHub Package
        // Registry cannot be read anonymously, so its observation stays in
        // repository-visible shell and only its completed document reaches the Action.
        retrieval: if primary { None } else { Some(retrieval) },
        observation_inputs,
        observation_inline: !primary,
    })
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
            ${@ENVVAR@SCOPE_ARGUMENTS[@]+"${@ENVVAR@SCOPE_ARGUMENTS[@]}"} 2>"${RUNNER_TEMP}/@JOB@npm-error")"; then
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

/// Cargo recipe: trusted publishing, a promotion gate, and cargo's own retrieval.
fn cargo_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, String> {
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
            .cargo()
            .and_then(|cargo| cargo.registry.as_ref())
            .and_then(|registry| registry.token_secret.as_deref())
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

    let mut observation_inputs =
        portable_observation_inputs(context, "cargo-crate", "cargo", "cargo", registry);
    observation_inputs.push_str(&format!(
        "      registry: {}\n      registry-name: {}\n      registry-index-variable: {}\n      registry-index-url: {}\n",
        scalar(registry),
        scalar(&registry_name),
        scalar(&if crates_io {
            String::new()
        } else {
            format!(
                "CARGO_REGISTRIES_{}_INDEX",
                environment_fragment(&registry_name)
            )
        }),
        scalar(&index),
    ));
    if !crates_io {
        observation_inputs.push_str("      observe: 'false'\n");
        steps.push_str(&inline_observation_step(
            context,
            "cargo-crate",
            "cargo",
            "cargo",
            registry,
            CARGO_OBSERVER,
            &format!(
                "      INPUT_REGISTRY: {}\n      INPUT_REGISTRY_NAME: {}\n      INPUT_REGISTRY_INDEX_VARIABLE: {}\n      INPUT_REGISTRY_INDEX_URL: {}\n      INPUT_CARRIED_TOKEN: {}\n      {}: ${{{{ secrets.{bootstrap} }}}}\n",
                scalar(registry),
                scalar(&registry_name),
                scalar(&format!(
                    "CARGO_REGISTRIES_{}_INDEX",
                    environment_fragment(&registry_name)
                )),
                scalar(&index),
                scalar(&carried),
                carried,
            ),
        ));
    }
    Ok(RecipeSteps {
        publisher: steps,
        // An alternate registry has no maintained read-only identity to mint.
        // Its authenticated observer therefore spends the existing publisher
        // credential inline rather than copying write authority to another job
        // or handing it to the first-party Action.
        retrieval: None,
        observation_inputs,
        observation_inline: !crates_io,
    })
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
            cargo add --quiet ${@ENVVAR@REGISTRY_ARGUMENTS[@]+"${@ENVVAR@REGISTRY_ARGUMENTS[@]}"} \
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
      cargo publish --locked --no-verify ${@ENVVAR@REGISTRY_ARGUMENTS[@]+"${@ENVVAR@REGISTRY_ARGUMENTS[@]}"}
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
fn oci_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, StepsRefusal> {
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

fn oci_destination_steps(context: &RecipeContext<'_>) -> Result<RecipeSteps, String> {
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
    let kind = if feature {
        "dev-container-feature"
    } else {
        "oci-image"
    };
    let promote = if feature {
        [
            OCI_EXISTING,
            DEV_CONTAINER_CONFLICT_GATE,
            OCI_CONFLICT_REPORT,
            DEV_CONTAINER_CONFLICT_TAIL,
            "      if [ \"${INTENTIONAL_PUBLISH}\" = yes ]; then\n        devcontainer features publish --namespace \"${@ENVVAR@FEATURE_NAMESPACE}\" .\n",
        ]
        .concat()
    } else {
        [
            OCI_IMAGE_PROMOTE_HEAD,
            OCI_EXISTING,
            OCI_IMAGE_CONFLICT_GATE,
            OCI_CONFLICT_REPORT,
            "      fi\n      if [ \"${INTENTIONAL_PUBLISH}\" = yes ]; then\n        crane push --index \"${layout}\" \"${repository}:${version}\"\n",
        ]
        .concat()
    };
    let signature = if signed {
        "      published=\"$(crane digest \"${repository}:${version}\")\"\n      cosign sign --yes \"${repository}@${published}\"\n"
    } else {
        ""
    };
    steps.push_str(&format!(
        "  - name: {}\n    working-directory: {}\n    env:\n{}      @ENVVAR@WORK: {}\n{}{namespace}    run: |\n{}{}{}{}{}{}{}",
        scalar(&format!("Publish {identity}")),
        scalar(context.working_directory),
        subject_environment(context),
        scalar(context.work),
        destination,
        STRICT_MODE,
        OCI_PROLOGUE,
        promote,
        if feature { "" } else { OCI_ALIAS_ENTITLEMENT },
        if feature { "" } else { OCI_ALIAS_PROMOTE },
        signature,
        "      fi\n",
    ));
    let observed_destination = publication.destination.clone().unwrap_or_else(|| {
        if feature {
            format!("${{{{ github.repository }}}}/{}", context.subject_identity)
        } else {
            format!(
                "${{{{ github.repository_owner }}}}/{}",
                context.subject_identity
            )
        }
    });
    let mut observation_inputs = portable_observation_inputs(
        context,
        kind,
        publication.packager.as_str(),
        "crane",
        &observed_destination,
    );
    let registry = if publication.target == "dockerhub" {
        "docker.io"
    } else {
        "ghcr.io"
    };
    observation_inputs.push_str(&format!(
        "      registry: {}\n      components: {}\n",
        scalar(registry),
        scalar(&components),
    ));
    Ok(RecipeSteps {
        publisher: steps,
        retrieval: None,
        observation_inputs,
        observation_inline: false,
    })
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
    let oci = context.unit.oci();
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
      provenance_file="${@ENVVAR@WORK}/provenance.yml"
      : > "${aliases_file}"
      : > "${metadata_file}"
      : > "${provenance_file}"
      INTENTIONAL_PUBLISH=yes
      printf '%s' "${@ENVVAR@REGISTRY_TOKEN}" | crane auth login "${@ENVVAR@REGISTRY}" \
        --username "${@ENVVAR@REGISTRY_USER}" --password-stdin
"#;

/// Read what the destination already holds under the released version.
///
/// Existence is decided from the destination's tag listing. The Open Container
/// Initiative Distribution API defines `NAME_UNKNOWN` for a repository name the
/// registry does not know. Whether crane preserves that code in diagnostics is
/// UNMEASURED until observed against the pinned client and a live registry. The
/// recipe assumes it does and treats only that code as empty; every other error
/// stops before the immutable-version conflict gate can be bypassed.
const OCI_EXISTING: &str = r#"      listing_error="${@ENVVAR@WORK}/listing-error"
      if ! known_tags="$(crane ls "${repository}" 2>"${listing_error}")"; then
        if grep -Eq '(^|[^A-Z_])NAME_UNKNOWN([^A-Z_]|$)' "${listing_error}"; then
          known_tags=""
        else
          cat "${listing_error}" >&2
          exit 1
        fi
      fi
      existing=""
      if printf '%s\n' "${known_tags}" | grep -Fxq "${version}"; then
        existing="$(crane digest "${repository}:${version}")"
      fi
"#;

/// Report a destination holding another subject, without touching it.
const OCI_CONFLICT_REPORT: &str = r#"        printf '%s already holds %s under version %s, which is not the subject this release built\n' \
          "${repository}" "${conflicting}" "${version}" >&2
        INTENTIONAL_PUBLISH=no
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
