// ---
// relationships:
//   implements: github-release-executor
// ---

//! Release-unit capability derivation and maintained publication recipe selection.

use super::names;
use crate::config::{discovery_candidate_directory, Config, PackageConfig, ReleaseUnitConfig};
use crate::error::{Error, Result};
use crate::evidence::assemble::CleanClientMode;
use crate::init::{
    detector_candidates, publication_detector_for_path, DiscoveryCandidate, SourceEvidence,
};
use crate::model::{Adapter, AttachedComponent, PublisherKind, ReleaseUnitDisposition};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

#[path = "recipe/publishers/cargo.rs"]
mod cargo_publisher;
#[path = "recipe/publishers/goreleaser.rs"]
mod goreleaser_publisher;
#[path = "recipe/publishers/npm.rs"]
mod npm_publisher;

pub(crate) use cargo_publisher::cargo_binary_identity;
use cargo_publisher::{cargo_manifest, cargo_registry};
use goreleaser_publisher::aur_package;
pub(crate) use goreleaser_publisher::go_main_package_directories;
use npm_publisher::node_package_is_publishable;

/// Canonical target identity of an adapter's implicit primary destination.
pub const PRIMARY_TARGET: &str = "primary";

/// Publishable characteristic derived from a release unit's native project evidence.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Publishable npm package.
    NodePackage,
    /// Publishable Cargo crate.
    RustCrate,
    /// Go module containing a main package.
    GoApplication,
    /// Dockerfile-backed runnable image.
    RunnableImage,
    /// Dev Container Feature.
    DevContainerFeature,
}

impl Capability {
    /// Every capability a release unit can derive.
    pub const ALL: [Self; 5] = [
        Self::NodePackage,
        Self::RustCrate,
        Self::GoApplication,
        Self::RunnableImage,
        Self::DevContainerFeature,
    ];

    /// Stable capability name used in plans and diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NodePackage => "node-package",
            Self::RustCrate => "rust-crate",
            Self::GoApplication => "go-application",
            Self::RunnableImage => "runnable-image",
            Self::DevContainerFeature => "dev-container-feature",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Native tool a maintained recipe uses to build and distribute a subject.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Packager {
    /// npm command-line client.
    Npm,
    /// Cargo command-line client.
    Cargo,
    /// Cargo-built native archive consumed by a Homebrew formula.
    CargoArchive,
    /// GoReleaser.
    GoReleaser,
    /// Docker Buildx.
    Buildx,
    /// Dev Container CLI.
    DevContainerCli,
}

impl Packager {
    /// Stable packager name used in plans and diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Cargo => "cargo",
            Self::CargoArchive => "cargo-archive",
            Self::GoReleaser => "goreleaser",
            Self::Buildx => "buildx",
            Self::DevContainerCli => "devcontainer-cli",
        }
    }

    /// Release-unit-relative paths that satisfy this packager's native configuration.
    pub const fn configuration_paths(self) -> &'static [&'static str] {
        match self {
            Self::Npm => &["package.json"],
            Self::Cargo | Self::CargoArchive => &["Cargo.toml"],
            Self::GoReleaser => &[".goreleaser.yaml", ".goreleaser.yml"],
            Self::Buildx => &["Dockerfile"],
            Self::DevContainerCli => &["devcontainer-feature.json"],
        }
    }

    /// Whether initialization may create this packager's baseline configuration.
    pub const fn baseline_is_authorable(self) -> bool {
        matches!(self, Self::GoReleaser)
    }
}

impl fmt::Display for Packager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One maintained publication recipe indexed by capability, packager, publisher, and target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recipe {
    /// Release-unit capability the recipe publishes.
    pub capability: Capability,
    /// Packager the recipe drives.
    pub packager: Packager,
    /// Publisher adapter the recipe implements.
    pub publisher: PublisherKind,
    /// Concrete or canonical target identity.
    pub target: &'static str,
    /// Attached components the recipe can produce and therefore omit.
    pub components: &'static [AttachedComponent],
    /// Consumer retrieval this recipe's destination admits.
    ///
    /// The design gives the recipe authority over consumer retrieval, and the
    /// mode belongs to the destination rather than to the adapter: one npm
    /// publisher reaches npmjs, which serves anonymous clients, and GitHub
    /// Package Registry, which serves none. Deriving the required mode from the
    /// publisher alone would force one of those two destinations to record a
    /// retrieval that did not happen the way it says it did.
    pub retrieval: CleanClientMode,
}

const OCI_COMPONENTS: &[AttachedComponent] = &[
    AttachedComponent::Sbom,
    AttachedComponent::Provenance,
    AttachedComponent::Signature,
];

const SIGNATURE_ONLY: &[AttachedComponent] = &[AttachedComponent::Signature];

const CATALOG: &[Recipe] = &[
    Recipe {
        capability: Capability::NodePackage,
        packager: Packager::Npm,
        publisher: PublisherKind::Npm,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::NodePackage,
        packager: Packager::Npm,
        publisher: PublisherKind::Npm,
        target: "github",
        components: &[],
        retrieval: CleanClientMode::AuthenticatedRegistry,
    },
    Recipe {
        capability: Capability::RustCrate,
        packager: Packager::Cargo,
        publisher: PublisherKind::Cargo,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::RustCrate,
        packager: Packager::CargoArchive,
        publisher: PublisherKind::Homebrew,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::AuthenticatedDraft,
    },
    Recipe {
        capability: Capability::RustCrate,
        packager: Packager::CargoArchive,
        publisher: PublisherKind::Rpm,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::RustCrate,
        packager: Packager::CargoArchive,
        publisher: PublisherKind::Apt,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::RustCrate,
        packager: Packager::CargoArchive,
        publisher: PublisherKind::Aur,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::AuthenticatedDraft,
    },
    Recipe {
        capability: Capability::GoApplication,
        packager: Packager::GoReleaser,
        publisher: PublisherKind::Homebrew,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::AuthenticatedDraft,
    },
    Recipe {
        capability: Capability::GoApplication,
        packager: Packager::GoReleaser,
        publisher: PublisherKind::Rpm,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::GoApplication,
        packager: Packager::GoReleaser,
        publisher: PublisherKind::Apt,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::GoApplication,
        packager: Packager::GoReleaser,
        publisher: PublisherKind::Aur,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::AuthenticatedDraft,
    },
    Recipe {
        capability: Capability::RunnableImage,
        packager: Packager::Buildx,
        publisher: PublisherKind::Oci,
        target: "dockerhub",
        components: OCI_COMPONENTS,
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::RunnableImage,
        packager: Packager::Buildx,
        publisher: PublisherKind::Oci,
        target: "ghcr",
        components: OCI_COMPONENTS,
        retrieval: CleanClientMode::Public,
    },
    Recipe {
        capability: Capability::DevContainerFeature,
        packager: Packager::DevContainerCli,
        publisher: PublisherKind::Oci,
        target: "ghcr",
        components: SIGNATURE_ONLY,
        retrieval: CleanClientMode::Public,
    },
];

/// Every maintained publication recipe.
pub fn catalog() -> &'static [Recipe] {
    CATALOG
}

/// Recipes that can publish one derived capability set through a publisher.
pub fn recipes_for(capabilities: &BTreeSet<Capability>, publisher: PublisherKind) -> Vec<Recipe> {
    CATALOG
        .iter()
        .copied()
        .filter(|recipe| recipe.publisher == publisher && capabilities.contains(&recipe.capability))
        .collect()
}

/// One release unit's derived capability and the exact evidence supporting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEvidence {
    /// Configured package whose artifact supplied the evidence.
    pub package: String,
    /// Derived capability.
    pub capability: Capability,
    /// Native artifact proving the capability.
    pub evidence: SourceEvidence,
}

/// One publishable native artifact available to become a configured package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCandidateEvidence {
    /// Detector that owns the artifact identity.
    pub detector: String,
    /// Derived publication capability.
    pub capability: Capability,
    /// Exact artifact evidence used to pin an accept or decline receipt.
    pub evidence: SourceEvidence,
    /// Package directory relative to the workspace root.
    pub directory: PathBuf,
    /// Manifest-native identity, when the detector extracts one.
    pub native_identity: Option<String>,
}

/// One configured publication resolved to exactly one maintained recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPublication {
    /// Configured release unit.
    pub release_unit: String,
    /// Configured package.
    pub package: String,
    /// Publisher adapter carrying the publication intent.
    pub publisher: PublisherKind,
    /// Canonical target identity recorded by publisher evidence.
    pub target: String,
    /// Concrete destination, when configuration or native evidence determines it.
    pub destination: Option<String>,
    /// Capability the selected recipe publishes.
    pub capability: Capability,
    /// Packager the selected recipe drives.
    pub packager: Packager,
    /// Components the recipe produces for this target.
    pub components: Vec<AttachedComponent>,
    /// Consumer retrieval the selected recipe's destination admits.
    pub retrieval: CleanClientMode,
    /// Configured observation deadline for a repository-defined destination.
    pub observation_deadline: Option<u64>,
}

impl SelectedPublication {
    /// Stable identity used by diagnostics and evidence.
    pub fn identity(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.release_unit, self.package, self.publisher, self.target
        )
    }
}

/// Every configured publication that resolved, and every one that did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicationSelection {
    /// Publications resolved to exactly one maintained recipe.
    pub selected: Vec<SelectedPublication>,
    /// Stable diagnostics for publications that could not be resolved.
    pub diagnostics: Vec<String>,
}

/// Whether one release unit's publications are resolved at all.
///
/// A suspended release unit does not release, so it cannot publish, and a unit
/// that configures no publisher has nothing to resolve. The selection skips
/// both without opening anything, so this is also the predicate that decides
/// which units' probe files are ever read — and evidence assembly proves
/// exactly those reads against the release commit. Two callers asking the same
/// question differently is how the reader comes to name fewer paths than the
/// selection opens, which is a gap no test of either side alone can see.
pub fn selects_publications(release_unit: &ReleaseUnitConfig) -> bool {
    release_unit.disposition == ReleaseUnitDisposition::Managed
        && !release_unit.publishers().is_empty()
}

/// Resolve every configured publication, collecting each package and target
/// failure instead of stopping at the first.
pub fn resolve_publications(root: &Path, config: &Config) -> Result<PublicationSelection> {
    let mut selection = PublicationSelection::default();
    for (id, release_unit) in &config.release_units {
        if !selects_publications(release_unit) {
            continue;
        }
        let mut owners = BTreeMap::<PathBuf, String>::new();
        for (package_id, package) in &release_unit.packages {
            let derived = match validate_package_artifact(
                root,
                config,
                id,
                release_unit,
                package_id,
                package,
                &mut owners,
            ) {
                Ok(derived) => derived,
                Err(Error::Validation(message)) => {
                    selection.diagnostics.push(message);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let capabilities = capability_set(&derived);
            for (publisher, target, configured) in configured_targets(package) {
                let context = SelectionContext {
                    root,
                    id,
                    package_id,
                    release_unit,
                    package,
                    capabilities: &capabilities,
                };
                match select_one(&context, publisher, target, &configured) {
                    Ok(publication) => selection.selected.push(publication),
                    Err(Error::Validation(message)) => selection.diagnostics.push(message),
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(selection)
}

/// Resolve one package artifact and reject shared ownership without preventing
/// the remaining package declarations from being checked.
fn validate_package_artifact(
    root: &Path,
    config: &Config,
    id: &str,
    release_unit: &ReleaseUnitConfig,
    package_id: &str,
    package: &PackageConfig,
    owners: &mut BTreeMap<PathBuf, String>,
) -> Result<Vec<CapabilityEvidence>> {
    let candidates = derive_package_capabilities(root, config, id, package_id, package)?;
    let artifact = match candidates.as_slice() {
            [evidence] => &evidence.evidence.path,
            [] => {
                let detected = matching_detector_candidates(root, config, id, package_id, package)?;
                let mut publishable = false;
                for candidate in &detected {
                    let capability = detector_capability(&candidate.detector)
                        .expect("matching candidates have package capabilities");
                    publishable |= candidate_is_publishable(root, candidate, capability)?;
                }
                let reason = if detected.is_empty()
                    && root.join(package_path(release_unit, package)).join("go.mod").is_file()
                {
                    "the package is a Go module but declares no discoverable main package".to_owned()
                } else if detected.is_empty() || publishable {
                    format!("evidence considered: {}", considered_candidate_paths(root, config, release_unit)?)
                } else {
                    format!(
                        "the matching manifest declines publication: {}",
                        detected.iter().map(|candidate| candidate.path.display().to_string()).collect::<Vec<_>>().join(", ")
                    )
                };
                return Err(Error::Validation(format!(
                    "release unit {id} package {package_id} at {} matches no publishable detector candidate; {reason}",
                    package_path(release_unit, package).display(),
                )))
            }
            many => {
                return Err(Error::Validation(format!(
                    "release unit {id} package {package_id} at {} matches {} publishable detector candidates: {}",
                    package_path(release_unit, package).display(),
                    many.len(),
                    many.iter()
                        .map(|evidence| format!("{} ({})", evidence.evidence.path.display(), evidence.capability))
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
    };
    if let Some(first) = owners.insert(artifact.clone(), package_id.to_owned()) {
        return Err(Error::Validation(format!(
            "release unit {id} packages {first} and {package_id} resolve to the same native artifact {}",
            artifact.display()
        )));
    }
    Ok(candidates)
}

/// Resolve every configured publication, failing on the first unresolved target.
pub fn select_publications(root: &Path, config: &Config) -> Result<Vec<SelectedPublication>> {
    let selection = resolve_publications(root, config)?;
    match selection.diagnostics.into_iter().next() {
        Some(diagnostic) => Err(Error::Validation(diagnostic)),
        None => Ok(selection.selected),
    }
}

/// Configured target identities and their target-scoped settings, in stable order.
fn configured_targets(package: &PackageConfig) -> Vec<(PublisherKind, String, Configured)> {
    let mut targets = Vec::new();
    if let Some(npm) = &package.npm {
        targets.push((
            PublisherKind::Npm,
            PRIMARY_TARGET.to_owned(),
            Configured {
                destination: Some("npmjs".to_owned()),
                ..Configured::default()
            },
        ));
        if npm
            .additional_targets
            .as_ref()
            .is_some_and(|targets| targets.github.is_some())
        {
            targets.push((
                PublisherKind::Npm,
                "github".to_owned(),
                Configured::default(),
            ));
        }
    }
    if package.cargo.is_some() {
        targets.push((
            PublisherKind::Cargo,
            PRIMARY_TARGET.to_owned(),
            Configured::default(),
        ));
    }
    if let Some(homebrew) = &package.homebrew {
        targets.push((
            PublisherKind::Homebrew,
            PRIMARY_TARGET.to_owned(),
            Configured {
                destination: Some(homebrew.repository.clone()),
                ..Configured::default()
            },
        ));
    }
    if let Some(rpm) = &package.rpm {
        targets.push((
            PublisherKind::Rpm,
            PRIMARY_TARGET.to_owned(),
            Configured {
                destination: Some(rpm.base_url.clone()),
                observation_deadline: Some(rpm.observation_deadline),
                ..Configured::default()
            },
        ));
    }
    if let Some(apt) = &package.apt {
        targets.push((
            PublisherKind::Apt,
            PRIMARY_TARGET.to_owned(),
            Configured {
                destination: Some(apt.base_url.clone()),
                observation_deadline: Some(apt.observation_deadline),
                ..Configured::default()
            },
        ));
    }
    if package.aur.is_some() {
        targets.push((
            PublisherKind::Aur,
            PRIMARY_TARGET.to_owned(),
            Configured::default(),
        ));
    }
    if let Some(oci) = &package.oci {
        if let Some(dockerhub) = &oci.dockerhub {
            targets.push((
                PublisherKind::Oci,
                "dockerhub".to_owned(),
                Configured {
                    destination: dockerhub.repository.clone(),
                    omit: dockerhub.omit.clone(),
                    destination_required: true,
                    ..Configured::default()
                },
            ));
        }
        if let Some(ghcr) = &oci.ghcr {
            targets.push((
                PublisherKind::Oci,
                "ghcr".to_owned(),
                Configured {
                    destination: ghcr.repository.clone(),
                    omit: ghcr.omit.clone(),
                    destination_required: false,
                    ..Configured::default()
                },
            ));
        }
    }
    targets
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn configured_target_identities(
    package: &PackageConfig,
) -> Vec<(PublisherKind, String)> {
    configured_targets(package)
        .into_iter()
        .map(|(publisher, target, _)| (publisher, target))
        .collect()
}

/// Kind of repository setting that holds one long-lived credential.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredCredentialKind {
    /// Repository variable.
    RepositoryVariable,
    /// Repository secret.
    RepositorySecret,
}

/// One destination whose maintained recipe reads a stored credential on every publication.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StandingCredentialDestination {
    /// Catalog route with a standing stored credential.
    Catalog(PublisherKind, String),
    /// Cargo primary route when `Cargo.toml` names a registry other than crates.io.
    CargoAlternateRegistry,
}

#[cfg(any(test, feature = "test-support"))]
impl StandingCredentialDestination {
    /// Reader-facing label the usage guide names for this destination.
    #[must_use]
    pub fn usage_label(&self) -> &'static str {
        match self {
            Self::Catalog(PublisherKind::Oci, target) if target == "dockerhub" => "Docker Hub",
            Self::Catalog(PublisherKind::Aur, _) => "the AUR",
            Self::CargoAlternateRegistry => "a non-crates.io Cargo registry",
            other => panic!("unexpected standing credential destination: {other:?}"),
        }
    }
}

/// Stored credentials the authority transition reads before minting repository-write tokens.
///
/// Init reports these names, and every maintained repository-write token step
/// reads the App ID from repository variables and the private key from repository
/// secrets. Minted installation tokens and job tokens are excluded.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn long_lived_repository_write_credentials() -> Vec<(StoredCredentialKind, String)> {
    use crate::config::DEFAULT_ENVVAR_PREFIX;
    vec![
        (
            StoredCredentialKind::RepositoryVariable,
            format!("{}GITHUB_APP_ID", DEFAULT_ENVVAR_PREFIX),
        ),
        (
            StoredCredentialKind::RepositorySecret,
            format!("{}GITHUB_APP_PRIVATE_KEY", DEFAULT_ENVVAR_PREFIX),
        ),
    ]
}

/// Publication routes whose maintained recipes implement registry trusted publishing.
///
/// Both routes are catalog primaries: npmjs and crates.io. Alternate Cargo
/// registries have no trusted-publishing exchange and are excluded.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn trusted_publishing_bootstrap_destinations() -> Vec<(PublisherKind, String)> {
    catalog()
        .iter()
        .filter(|recipe| {
            matches!(
                (recipe.publisher, recipe.target),
                (PublisherKind::Npm, PRIMARY_TARGET) | (PublisherKind::Cargo, PRIMARY_TARGET)
            )
        })
        .map(|recipe| (recipe.publisher, recipe.target.to_owned()))
        .collect()
}

/// Destinations whose maintained recipes read a stored credential on every publication.
///
/// Docker Hub and the AUR are catalog routes. A non-crates.io Cargo registry is
/// the configuration partition where `cargo_steps` emits token authentication
/// rather than trusted publishing.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn standing_credential_destinations() -> Vec<StandingCredentialDestination> {
    let routes = [
        (PublisherKind::Oci, "dockerhub"),
        (PublisherKind::Aur, PRIMARY_TARGET),
    ];
    for (publisher, target) in routes {
        assert!(
            catalog()
                .iter()
                .any(|recipe| recipe.publisher == publisher && recipe.target == target),
            "the catalog admits the {publisher} {target} standing-credential route"
        );
    }
    vec![
        StandingCredentialDestination::Catalog(PublisherKind::Oci, "dockerhub".to_owned()),
        StandingCredentialDestination::Catalog(PublisherKind::Aur, PRIMARY_TARGET.to_owned()),
        StandingCredentialDestination::CargoAlternateRegistry,
    ]
}

#[derive(Debug, Default, Clone)]
struct Configured {
    destination: Option<String>,
    omit: Vec<AttachedComponent>,
    destination_required: bool,
    observation_deadline: Option<u64>,
}

struct SelectionContext<'a> {
    root: &'a Path,
    id: &'a str,
    package_id: &'a str,
    release_unit: &'a ReleaseUnitConfig,
    package: &'a PackageConfig,
    capabilities: &'a BTreeSet<Capability>,
}

fn select_one(
    context: &SelectionContext<'_>,
    publisher: PublisherKind,
    target: String,
    configured: &Configured,
) -> Result<SelectedPublication> {
    let SelectionContext {
        root,
        id,
        package_id,
        release_unit,
        package,
        capabilities,
    } = context;
    names::release_unit(&names::SuppliedName {
        origin: &format!("release unit {id}"),
        value: id,
    })
    .map_err(Error::Validation)?;
    names::package(&names::SuppliedName {
        origin: &format!("release unit {id} package {package_id}"),
        value: package_id,
    })
    .map_err(Error::Validation)?;
    let matches = recipes_for(capabilities, publisher)
        .into_iter()
        .filter(|recipe| recipe.target == target)
        .collect::<Vec<_>>();
    let recipe = match matches.as_slice() {
        [recipe] => *recipe,
        [] => {
            // A capability the release unit nearly derived is the cause the
            // user can act on. Reporting only the derived set names the
            // symptom, and for a Go module whose command package was not
            // discovered it names the wrong thing entirely.
            if let Some(reason) = withheld_capability_reason(root, release_unit, capabilities)? {
                return Err(Error::Validation(format!(
                    "no maintained publication recipe matches the configured target {id}/{package_id}/{publisher}/{target}: {reason}"
                )));
            }
            return Err(Error::Validation(format!(
                "no maintained publication recipe matches the configured target {id}/{package_id}/{publisher}/{target}; derived capabilities are {}",
                capability_names(capabilities)
            )))
        }
        many => {
            return Err(Error::Validation(format!(
                "configured target {id}/{package_id}/{publisher}/{target} matches {} maintained recipes ({}); the package must derive one publishable capability",
                many.len(),
                many.iter()
                    .map(|recipe| recipe.capability.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
    };
    for component in &configured.omit {
        if !recipe.components.contains(component) {
            return Err(Error::Validation(format!(
                "omitted component {component} is not supported by the target recipe for {id}/{package_id}/{publisher}/{target}"
            )));
        }
    }
    let destination = match (&configured.destination, publisher) {
        (Some(destination), _) => Some(destination.clone()),
        (None, PublisherKind::Cargo) => Some(cargo_registry(root, release_unit, package)?),
        // The Arch User Repository resolves a package by name, and GoReleaser's
        // own configuration is where that name lives: explicitly under `aur`,
        // and otherwise as the binary package of the declared project.
        (None, PublisherKind::Aur) => match recipe.packager {
            Packager::GoReleaser => aur_package(
                root,
                release_unit,
                &format!("{id}/{package_id}/{publisher}/{target}"),
            )?,
            Packager::CargoArchive => {
                let directory = package_path(release_unit, package);
                let binary = cargo_binary_identity(
                    root,
                    &directory,
                    &format!("{id}/{package_id}/{publisher}/{target}"),
                )
                .map_err(Error::Validation)?;
                let origin = format!("{} binary name", directory.join("Cargo.toml").display());
                crate::executor::goreleaser::arch_package_name(
                    None,
                    &names::SuppliedName {
                        origin: &origin,
                        value: &binary,
                    },
                )
                .map(Some)
                .map_err(Error::Validation)?
            }
            _ => None,
        },
        (None, _) if configured.destination_required => {
            return Err(Error::Validation(format!(
                "configured target {id}/{package_id}/{publisher}/{target} requires an explicit repository; its destination identity is not derivable"
            )))
        }
        (None, _) => None,
    };
    let retrieval = retrieval_mode(&recipe, destination.as_deref());
    Ok(SelectedPublication {
        release_unit: (*id).to_owned(),
        package: (*package_id).to_owned(),
        publisher,
        target,
        destination,
        capability: recipe.capability,
        packager: recipe.packager,
        components: recipe
            .components
            .iter()
            .copied()
            .filter(|component| !configured.omit.contains(component))
            .collect(),
        retrieval,
        observation_deadline: configured.observation_deadline,
    })
}

/// Retrieval one selected recipe's resolved destination admits.
///
/// A recipe whose destination the catalog names fixes the mode outright. Cargo
/// does not: its one primary target resolves to whatever registry
/// `package.publish` names, and the maintained recipe authenticates every
/// registry that is not crates.io with a configured token. Such a registry is
/// ordinarily a private or internal one, and the retrieval the recipe performs
/// against it runs with that credential in its environment, so recording the
/// catalog's public default there would assert a consumer path nobody outside
/// the credential holder could take — the exact untruth the authenticated mode
/// exists to prevent.
fn retrieval_mode(recipe: &Recipe, destination: Option<&str>) -> CleanClientMode {
    match (recipe.publisher, destination) {
        (PublisherKind::Cargo, Some(CRATES_IO) | None) => recipe.retrieval,
        (PublisherKind::Cargo, Some(_)) => CleanClientMode::AuthenticatedRegistry,
        _ => recipe.retrieval,
    }
}

/// Why a release unit that looks publishable derived no capability for it.
///
/// Native evidence that a project is a Go module is not evidence that it is a
/// publishable Go application: the packager builds a command, and a module with
/// no discoverable command package has nothing for it to build. That distinction
/// is invisible in the derived capability set, so it is reported here rather
/// than leaving the user to read "derived capabilities are none" and conclude
/// the release unit is not a Go project at all.
fn withheld_capability_reason(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
    capabilities: &BTreeSet<Capability>,
) -> Result<Option<String>> {
    if capabilities.contains(&Capability::GoApplication) {
        return Ok(None);
    }
    let directory = root.join(&release_unit.path);
    if !directory.join("go.mod").is_file() {
        return Ok(None);
    }
    if !go_main_package_directories(&directory)?.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "release unit {} is a Go module but declares no discoverable main package, so it derives no {} capability; the maintained recipes build a command, and the packager searches module directories included by Go's ./... package pattern for package main",
        release_unit.path.display(),
        Capability::GoApplication
    )))
}

fn capability_names(capabilities: &BTreeSet<Capability>) -> String {
    if capabilities.is_empty() {
        return "none".to_owned();
    }
    capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Capability identities from derived evidence.
pub fn capability_set(evidence: &[CapabilityEvidence]) -> BTreeSet<Capability> {
    evidence.iter().map(|item| item.capability).collect()
}

/// Every path the publication selection opens, relative to the workspace root.
///
/// A consumer that must prove what the selection read needs the paths before
/// the selection runs. Existing paths come from detector evidence. Paths only
/// the release commit carries are classified through the same detector
/// predicates by evidence assembly.
pub fn publication_probe_paths(root: &Path, config: &Config) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    let publishing_units = config
        .release_units
        .values()
        .filter(|release_unit| selects_publications(release_unit))
        .collect::<Vec<_>>();
    if publishing_units.is_empty() {
        return Ok(paths);
    }
    for candidate in detector_candidates(root)? {
        let candidate_path = discovery_candidate_directory(&candidate.detector, &candidate.path);
        if !publishing_units
            .iter()
            .any(|release_unit| path_contains(&release_unit.path, &candidate_path))
        {
            continue;
        }
        paths.extend(
            candidate
                .evidence
                .into_iter()
                .map(|evidence| evidence.path)
                // A Go command candidate carries a digest for its directory as
                // well as its member files. Only files have release blobs to
                // compare, so the directory evidence is deliberately omitted.
                .filter(|path| root.join(path).is_file()),
        );
    }
    for release_unit in publishing_units {
        collect_go_source_paths(
            &root.join(&release_unit.path),
            &release_unit.path,
            &mut paths,
        );
        if release_unit.aur().is_some() {
            for name in Packager::GoReleaser.configuration_paths() {
                let path = release_unit.path.join(name);
                paths.insert(path.clone());
                if root.join(path).is_file() {
                    break;
                }
            }
        }
    }
    Ok(paths)
}

/// Capability whose detector recognizes one publication manifest path.
///
/// The detector owns filename and variant semantics. The exhaustive capability
/// match binds each detector identity to the catalog key it can derive, so a
/// new capability cannot compile without joining this mapping.
pub(crate) fn publication_manifest_capability(path: &Path) -> Option<Capability> {
    let detector = publication_detector_for_path(path)?;
    Capability::ALL
        .into_iter()
        .find(|capability| match capability {
            Capability::NodePackage => detector == "npm-package",
            Capability::RustCrate => detector == "cargo-package",
            Capability::GoApplication => detector == "go-module",
            Capability::RunnableImage => detector == "docker-image",
            Capability::DevContainerFeature => detector == "devcontainer-feature",
        })
}

fn collect_go_source_paths(directory: &Path, relative: &Path, paths: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let child = relative.join(&name);
        let path = entry.path();
        if path.is_dir() {
            collect_go_source_paths(&path, &child, paths);
        } else if child.extension().is_some_and(|extension| extension == "go") {
            paths.insert(child);
        }
    }
}

/// Derive every publishable capability of one release unit from its native evidence.
pub fn derive_capabilities(
    root: &Path,
    config: &Config,
    release_unit_id: &str,
) -> Result<Vec<CapabilityEvidence>> {
    let release_unit = config.release_units.get(release_unit_id).ok_or_else(|| {
        Error::Validation(format!("release unit {release_unit_id} is not configured"))
    })?;
    let mut derived = Vec::new();
    if release_unit.packages.is_empty() {
        let package = PackageConfig::new(PathBuf::from("."));
        return derive_package_capabilities(root, config, release_unit_id, "", &package);
    }
    for (package_id, package) in &release_unit.packages {
        derived.extend(derive_package_capabilities(
            root,
            config,
            release_unit_id,
            package_id,
            package,
        )?);
    }
    Ok(derived)
}

/// Enumerate every live publishable artifact contained by one release unit.
pub fn derive_package_candidates(
    root: &Path,
    config: &Config,
    release_unit_id: &str,
) -> Result<Vec<PackageCandidateEvidence>> {
    let release_unit = config.release_units.get(release_unit_id).ok_or_else(|| {
        Error::Validation(format!("release unit {release_unit_id} is not configured"))
    })?;
    let managed = config
        .discovery
        .managed_paths
        .iter()
        .map(|receipt| (&receipt.detector, &receipt.path))
        .collect::<BTreeSet<_>>();
    live_detector_candidates(root, config)?
        .into_iter()
        .filter(|candidate| {
            let directory = discovery_candidate_directory(&candidate.detector, &candidate.path);
            path_contains(&release_unit.path, &directory)
                && !managed.contains(&(&candidate.detector, &candidate.path))
        })
        .filter_map(|candidate| {
            let capability = detector_capability(&candidate.detector)?;
            Some(
                candidate_belongs_to_release_unit(root, release_unit, &candidate, capability)
                    .and_then(|belongs| {
                        candidate_is_publishable(root, &candidate, capability)
                            .map(|publishable| belongs && publishable)
                    })
                    .map(|publishable| {
                        publishable.then(|| PackageCandidateEvidence {
                            detector: candidate.detector.clone(),
                            capability,
                            evidence: candidate
                                .evidence
                                .iter()
                                .find(|evidence| evidence.path == candidate.path)
                                .expect("discovery candidate validates exact-path evidence")
                                .clone(),
                            directory: discovery_candidate_directory(
                                &candidate.detector,
                                &candidate.path,
                            ),
                            native_identity: candidate.native_identity,
                        })
                    }),
            )
        })
        .collect::<Result<Vec<_>>>()
        .map(|items| items.into_iter().flatten().collect())
}

fn candidate_belongs_to_release_unit(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
    candidate: &DiscoveryCandidate,
    capability: Capability,
) -> Result<bool> {
    let adapter = match capability {
        Capability::NodePackage => Adapter::Npm,
        Capability::RustCrate => Adapter::Cargo,
        Capability::GoApplication => Adapter::Go,
        Capability::RunnableImage | Capability::DevContainerFeature => return Ok(true),
    };
    let projections = release_unit
        .projections
        .iter()
        .filter(|projection| {
            projection.adapter == adapter
                || match capability {
                    Capability::NodePackage => projection.file.ends_with("package.json"),
                    Capability::RustCrate => projection.file.ends_with("Cargo.toml"),
                    Capability::GoApplication => projection.file.ends_with("go.mod"),
                    Capability::RunnableImage | Capability::DevContainerFeature => false,
                }
        })
        .map(|projection| {
            if release_unit.path == Path::new(".") {
                projection.file.clone()
            } else {
                release_unit.path.join(&projection.file)
            }
        })
        .collect::<BTreeSet<_>>();
    if projections.contains(&candidate.path) {
        return Ok(true);
    }
    let absolute_root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| Error::io(root, error))?
            .join(root)
    };
    let release_root = absolute_root.join(&release_unit.path);
    let workspace_manifests = crate::init::workspace_manifest_paths(&release_root)?
        .into_iter()
        .map(|path| {
            path.strip_prefix(&absolute_root)
                .map(Path::to_owned)
                .map_err(|_| {
                    Error::Validation(format!(
                        "workspace manifest {} is outside release workspace {}",
                        path.display(),
                        absolute_root.display()
                    ))
                })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    match capability {
        Capability::RustCrate | Capability::NodePackage => {
            Ok(workspace_manifests.contains(&candidate.path))
        }
        Capability::GoApplication => Ok(candidate.evidence.iter().any(|evidence| {
            evidence
                .path
                .file_name()
                .is_some_and(|name| name == "go.mod")
                && (projections.contains(&evidence.path)
                    || evidence.path.parent() == Some(release_unit.path.as_path()))
        })),
        Capability::RunnableImage | Capability::DevContainerFeature => Ok(false),
    }
}

fn derive_package_capabilities(
    root: &Path,
    config: &Config,
    release_unit_id: &str,
    package_id: &str,
    package: &PackageConfig,
) -> Result<Vec<CapabilityEvidence>> {
    matching_detector_candidates(root, config, release_unit_id, package_id, package)?
        .into_iter()
        .filter_map(|candidate| {
            let capability = detector_capability(&candidate.detector)?;
            Some(
                candidate_is_publishable(root, &candidate, capability).map(|publishable| {
                    publishable.then(|| CapabilityEvidence {
                        package: package_id.to_owned(),
                        capability,
                        evidence: candidate
                            .evidence
                            .iter()
                            .find(|evidence| evidence.path == candidate.path)
                            .expect("discovery candidate validates exact-path evidence")
                            .clone(),
                    })
                }),
            )
        })
        .collect::<Result<Vec<_>>>()
        .map(|items| items.into_iter().flatten().collect())
}

fn matching_detector_candidates(
    root: &Path,
    config: &Config,
    release_unit_id: &str,
    package_id: &str,
    package: &PackageConfig,
) -> Result<Vec<DiscoveryCandidate>> {
    let release_unit = config.release_units.get(release_unit_id).ok_or_else(|| {
        Error::Validation(format!("release unit {release_unit_id} is not configured"))
    })?;
    let path = package_path(release_unit, package);
    let receipts = config
        .discovery
        .managed_paths
        .iter()
        .filter(|receipt| receipt.release_unit == release_unit_id && receipt.package == package_id)
        .map(|receipt| (&receipt.detector, &receipt.path))
        .collect::<BTreeSet<_>>();
    Ok(live_detector_candidates(root, config)?
        .into_iter()
        .filter(|candidate| {
            if receipts.is_empty() {
                let candidate_path =
                    discovery_candidate_directory(&candidate.detector, &candidate.path);
                path_contains(&path, &candidate_path)
            } else {
                receipts.contains(&(&candidate.detector, &candidate.path))
            }
        })
        .filter(|candidate| {
            if candidate.detector != "go-command" {
                return true;
            }
            let module_directory = candidate
                .evidence
                .iter()
                .find(|evidence| {
                    evidence
                        .path
                        .file_name()
                        .is_some_and(|name| name == "go.mod")
                })
                .and_then(|evidence| evidence.path.parent());
            module_directory.is_some_and(|module| path.starts_with(module))
        })
        .filter(|candidate| detector_capability(&candidate.detector).is_some())
        .collect())
}

fn package_path(release_unit: &ReleaseUnitConfig, package: &PackageConfig) -> PathBuf {
    crate::config::join_relative_paths(&release_unit.path, &package.path)
}

fn path_contains(parent: &Path, child: &Path) -> bool {
    parent == Path::new(".") || child == parent || child.starts_with(parent)
}

fn detector_capability(detector: &str) -> Option<Capability> {
    match detector {
        "npm-package" => Some(Capability::NodePackage),
        "cargo-package" => Some(Capability::RustCrate),
        "go-command" => Some(Capability::GoApplication),
        "docker-image" => Some(Capability::RunnableImage),
        "devcontainer-feature" => Some(Capability::DevContainerFeature),
        _ => None,
    }
}

fn candidate_is_publishable(
    root: &Path,
    candidate: &DiscoveryCandidate,
    capability: Capability,
) -> Result<bool> {
    match capability {
        Capability::NodePackage => node_package_is_publishable(root, &candidate.path),
        Capability::RustCrate => {
            Ok(cargo_manifest(root, &candidate.path)?
                .is_some_and(|manifest| manifest.publishable()))
        }
        Capability::GoApplication | Capability::RunnableImage | Capability::DevContainerFeature => {
            Ok(true)
        }
    }
}

fn live_detector_candidates(root: &Path, config: &Config) -> Result<Vec<DiscoveryCandidate>> {
    let excluded = config
        .discovery
        .excluded_paths
        .iter()
        .map(|receipt| ((&receipt.detector, &receipt.path), &receipt.evidence_digest))
        .collect::<BTreeMap<_, _>>();
    Ok(detector_candidates(root)?
        .into_iter()
        .filter(|candidate| {
            let Some(expected) = excluded.get(&(&candidate.detector, &candidate.path)) else {
                return true;
            };
            candidate
                .evidence
                .iter()
                .find(|evidence| evidence.path == candidate.path)
                .is_none_or(|evidence| &evidence.digest != *expected)
        })
        .collect())
}

fn considered_candidate_paths(
    root: &Path,
    config: &Config,
    release_unit: &ReleaseUnitConfig,
) -> Result<String> {
    let paths = live_detector_candidates(root, config)?
        .into_iter()
        .filter(|candidate| detector_capability(&candidate.detector).is_some())
        .map(|candidate| candidate.path)
        .filter(|path| path_contains(&release_unit.path, path))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    Ok(if paths.is_empty() {
        "none".to_owned()
    } else {
        paths.join(", ")
    })
}

/// Default Cargo registry when a manifest states no explicit destination.
const CRATES_IO: &str = "crates.io";

#[cfg(test)]
thread_local! {
    static PROBED_FILE_BEFORE_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
struct ProbedFileBeforeReadHook;

#[cfg(test)]
impl ProbedFileBeforeReadHook {
    fn install(before_read: impl FnOnce() + 'static) -> Self {
        PROBED_FILE_BEFORE_READ.with(|slot| {
            assert!(
                slot.replace(Some(Box::new(before_read))).is_none(),
                "only one probed-file hook may be installed on a test thread"
            );
        });
        Self
    }
}

#[cfg(test)]
impl Drop for ProbedFileBeforeReadHook {
    fn drop(&mut self) {
        PROBED_FILE_BEFORE_READ.with(|slot| {
            slot.replace(None);
        });
    }
}

/// Read one probed file, or `None` when it is not there to be read.
///
/// A capability probe asks whether a path is a file and then reads it, and the
/// two questions are asked at different instants. Executor tests derive
/// capabilities against temporary workspaces, and a workspace dropped on
/// another thread can take the file inside that window. A file that has gone
/// by the time the read reaches it is the absent case — the same answer the
/// existence check itself would have given a moment later — rather than an
/// unexplained missing-file error naming a path nobody can inspect any more.
/// Every other read failure is still reported.
fn probed_file_text(path: &Path) -> Result<Option<String>> {
    #[cfg(test)]
    PROBED_FILE_BEFORE_READ.with(|before_read| {
        if let Some(before_read) = before_read.borrow_mut().take() {
            before_read();
        }
    });

    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(path, error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;

    include!("recipe/tests/npm.rs");
    include!("recipe/tests/cargo.rs");
    include!("recipe/tests/goreleaser.rs");
    include!("recipe/tests/oci.rs");

    const GITHUB: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#;

    fn config(publisher: &str) -> Config {
        config_at(".", publisher)
    }

    fn config_at(path: &str, publisher: &str) -> Config {
        let package = publisher
            .lines()
            .map(|line| format!("    {line}\n"))
            .collect::<String>();
        let text = GITHUB.replace(
            "    path: component\n",
            &format!(
                "    path: component\n    packages:\n      package:\n        path: {path}\n{package}"
            ),
        );
        Config::from_yaml(&text).expect("fixture config")
    }

    fn derive_component(root: &Path, config: &Config) -> Result<Vec<CapabilityEvidence>> {
        derive_capabilities(root, config, "component")
    }

    fn assert_publication_segment_is_refused(config: &str, expected_origin: &str) {
        let workspace = Workspace::new("publication-identity-segment");
        workspace.write(
            "component/package.json",
            r#"{"name":"sample-library","version":"1.0.0"}"#,
        );
        let config = Config::from_yaml(config).expect("fixture config");
        let selection = resolve_publications(workspace.root(), &config)
            .expect("invalid publication names are reported as diagnostics");
        assert!(
            selection.selected.is_empty(),
            "an invalid segment must not select a publication: {selection:?}"
        );
        let message = selection.diagnostics.join("\n");
        assert!(message.contains(expected_origin), "{message}");
        assert!(message.contains("is not a"), "{message}");
    }

    #[test]
    fn refuses_a_release_unit_identifier_containing_the_identity_separator() {
        assert_publication_segment_is_refused(
            &GITHUB
                .replace("  component:\n", "  component/part:\n")
                .replace(
                    "    path: component\n",
                    "    path: component\n    packages:\n      package:\n        path: .\n        npm: {}\n",
                ),
            "release unit component/part",
        );
    }

    #[test]
    fn refuses_a_package_identifier_containing_the_identity_separator() {
        assert_publication_segment_is_refused(
            &GITHUB.replace(
                "    path: component\n",
                "    path: component\n    packages:\n      package/part:\n        path: .\n        npm: {}\n",
            ),
            "release unit component package package/part",
        );
    }

    #[test]
    fn system_package_selection_fixes_public_retrieval_and_carries_configured_deadline() {
        let workspace = Workspace::new("system-package-selection");
        workspace
            .write("component/go.mod", "module example.test/sample-command\n")
            .write("component/main.go", "package main\nfunc main() {}\n")
            .write(
                "component/.goreleaser.yaml",
                "version: 2\nproject_name: sample-command\nbuilds: [ { main: . } ]\nnfpms: [ { formats: [ rpm ] } ]\n",
            );
        let config = config(
            "    rpm:\n      delivery-action: .github/actions/deliver\n      base-url: https://packages.invalid/rpm\n      public-signing-key-url: https://packages.invalid/key.asc\n      observation-deadline: 47\n      channel: stable\n      with: {}\n",
        );
        let selected = select_publications(workspace.root(), &config).expect("selection");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].retrieval, CleanClientMode::Public);
        assert_eq!(selected[0].observation_deadline, Some(47));
    }

    #[test]
    fn derives_capabilities_from_native_evidence_only() {
        let workspace = Workspace::new("capabilities");
        workspace
            .write(
                "component/package.json",
                r#"{"name":"example-component","version":"1.0.0"}"#,
            )
            .write("component/Dockerfile", "FROM scratch\n")
            .write("component/go.mod", "module example.test/component\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n");
        let config = config("");
        let derived = derive_component(workspace.root(), &config).expect("capabilities derive");
        assert_eq!(
            capability_set(&derived),
            BTreeSet::from([
                Capability::NodePackage,
                Capability::GoApplication,
                Capability::RunnableImage,
            ])
        );
        assert!(derived
            .iter()
            .all(|item| item.evidence.digest.starts_with("sha256:")));

        assert!(
            select_publications(workspace.root(), &config)
                .expect("no publishers selects nothing")
                .is_empty(),
            "native package evidence alone never creates publication intent"
        );
    }

    #[test]
    fn rejects_two_packages_that_resolve_to_one_native_artifact() {
        let workspace = Workspace::new("duplicate-native-artifact");
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"sample-crate\"\nversion = \"1.0.0\"\n",
        );
        let text = GITHUB.replace(
            "    path: component\n",
            "    path: component\n    packages:\n      first:\n        path: .\n        cargo: {}\n      second:\n        path: .\n        cargo: {}\n",
        );
        let config = Config::from_yaml(&text).expect("package declarations");
        let error = select_publications(workspace.root(), &config)
            .expect_err("one artifact cannot be owned twice");
        let message = error.to_string();
        assert!(message.contains("packages first and second"), "{message}");
        assert!(message.contains("component/Cargo.toml"), "{message}");
    }

    #[test]
    fn refuses_one_package_declaration_that_matches_distinct_native_artifacts() {
        let workspace = Workspace::new("coincident-package-paths");
        workspace
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"sample-crate\"\nversion = \"1.0.0\"\n",
            )
            .write(
                "component/package.json",
                r#"{"name":"sample-package","version":"1.0.0"}"#,
            );
        let text = GITHUB.replace(
            "    path: component\n",
            "    path: component\n    packages:\n      rust:\n        path: .\n        cargo: {}\n      node:\n        path: .\n        npm: {}\n",
        );
        let config = Config::from_yaml(&text).expect("package declarations");
        let error = select_publications(workspace.root(), &config)
            .expect_err("each coincident declaration remains ambiguous without receipts");
        assert!(
            error
                .to_string()
                .contains("matches 2 publishable detector candidates"),
            "{error}"
        );
    }

    #[test]
    fn managed_receipts_disambiguate_coincident_package_paths() {
        let workspace = Workspace::new("receipted-coincident-packages");
        workspace
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"sample-crate\"\nversion = \"1.0.0\"\n",
            )
            .write(
                "component/package.json",
                r#"{"name":"sample-package","version":"1.0.0"}"#,
            );
        let text = GITHUB.replace(
            "    path: component\n",
            "    path: component\n    packages:\n      rust:\n        path: .\n        cargo: {}\n      node:\n        path: .\n        npm: {}\n",
        );
        let mut config = Config::from_yaml(&text).expect("package declarations");
        config.discovery.managed_paths.extend([
            crate::config::ManagedPathReceipt {
                detector: "cargo-package".to_owned(),
                path: PathBuf::from("component/Cargo.toml"),
                release_unit: "component".to_owned(),
                package: "rust".to_owned(),
            },
            crate::config::ManagedPathReceipt {
                detector: "npm-package".to_owned(),
                path: PathBuf::from("component/package.json"),
                release_unit: "component".to_owned(),
                package: "node".to_owned(),
            },
        ]);

        let selected = select_publications(workspace.root(), &config)
            .expect("each receipt assigns one native artifact");
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected
                .iter()
                .map(|publication| publication.capability)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([Capability::NodePackage, Capability::RustCrate])
        );
    }

    /// Detector evidence is named only beneath publishing release units.
    ///
    /// Go's pattern-based source walk is proved end to end by evidence assembly
    /// tests. This assertion covers the fixed evidence members and the unit
    /// boundary without restating the old filename-probe mechanism.
    #[test]
    fn names_every_path_the_selection_opens() {
        let workspace = Workspace::new("detector-evidence-paths");
        workspace
            .write("component/go.mod", "module example.test/component\n")
            .write(
                "component/cmd/tool/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write("component/internal/helper.go", "package internal\n")
            .write("component/Dockerfile.alpine", "FROM scratch\n")
            .write("component/.goreleaser.yaml", "version: 2\n")
            .write("unrelated/package.json", r#"{"name":"example-package"}"#);
        let config = config_at(
            "cmd/tool",
            "    homebrew: { repository: example-org/homebrew-tap }\n",
        );
        let named = publication_probe_paths(workspace.root(), &config).expect("probe paths");
        assert_eq!(
            named,
            BTreeSet::from([
                PathBuf::from("component/Dockerfile.alpine"),
                PathBuf::from("component/cmd/tool/main.go"),
                PathBuf::from("component/go.mod"),
                PathBuf::from("component/internal/helper.go"),
            ]),
            "the reader is derived from every detector evidence member the selection scan opens"
        );
    }

    /// Every capability the maintained catalog indexes is reached by derivation.
    ///
    /// The expected set is extracted from the catalog itself. A route added
    /// later therefore enters this assertion without a second maintained list.
    #[test]
    fn derives_every_capability_indexed_by_the_catalog() {
        let workspace = Workspace::new("every-capability");
        workspace
            .write(
                "component/package.json",
                r#"{"name":"example-component","version":"1.0.0"}"#,
            )
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
            )
            .write("component/go.mod", "module example.test/component\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n")
            .write("component/Dockerfile", "FROM scratch\n")
            .write(
                "component/devcontainer-feature.json",
                "{\"id\":\"example\"}\n",
            );
        let derived = derive_component(workspace.root(), &config("")).expect("capabilities derive");
        let indexed = catalog()
            .iter()
            .map(|recipe| recipe.capability)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            capability_set(&derived),
            indexed,
            "detector evidence reaches every capability indexed by the catalog"
        );
    }

    #[test]
    fn treats_private_and_virtual_manifests_as_unpublishable() {
        let workspace = Workspace::new("unpublishable");
        workspace
            .write(
                "component/package.json",
                r#"{"name":"example-component","private":true}"#,
            )
            .write("component/Cargo.toml", "[workspace]\nmembers = []\n");
        let derived = derive_component(workspace.root(), &config("")).expect("capabilities derive");
        assert!(capability_set(&derived).is_empty());
    }

    #[test]
    fn reports_manifests_that_cannot_be_parsed() {
        let workspace = Workspace::new("malformed");
        workspace.write("component/package.json", "{ not json");
        let error = derive_component(workspace.root(), &config(""))
            .expect_err("malformed manifest reported");
        assert!(
            error.to_string().contains("invalid") && error.to_string().contains("package.json"),
            "{error}"
        );

        let workspace = Workspace::new("malformed-toml");
        workspace.write("component/Cargo.toml", "[package\nname =");
        let error = derive_component(workspace.root(), &config(""))
            .expect_err("malformed manifest reported");
        assert!(
            error.to_string().contains("invalid") && error.to_string().contains("Cargo.toml"),
            "{error}"
        );
    }

    #[test]
    fn selects_one_recipe_for_each_configured_target() {
        let workspace = Workspace::new("selection");
        workspace
            .write(
                "component/node/package.json",
                r#"{"name":"example-component","version":"1.0.0"}"#,
            )
            .write("component/image/Dockerfile", "FROM scratch\n");
        let text = GITHUB.replace(
            "    path: component\n",
            "    path: component\n    packages:\n      node:\n        path: node\n        npm: { additional-targets: { github: {} } }\n      image:\n        path: image\n        oci:\n          ghcr: { omit: [ signature ] }\n",
        );
        let config = Config::from_yaml(&text).expect("package declarations");
        let selected = select_publications(workspace.root(), &config).expect("publications select");
        let identities = selected
            .iter()
            .map(SelectedPublication::identity)
            .collect::<Vec<_>>();
        assert_eq!(
            identities,
            vec![
                "component/image/oci/ghcr".to_owned(),
                "component/node/npm/primary".to_owned(),
                "component/node/npm/github".to_owned(),
            ]
        );
        assert_eq!(selected[0].capability, Capability::RunnableImage);
        assert_eq!(
            selected[0].components,
            vec![AttachedComponent::Sbom, AttachedComponent::Provenance]
        );
        assert_eq!(selected[1].destination.as_deref(), Some("npmjs"));
        assert_eq!(selected[1].packager, Packager::Npm);
        assert_eq!(selected[2].destination, None);
    }

    #[test]
    fn withholds_publication_from_suspended_release_units() {
        let workspace = Workspace::new("suspended");
        workspace.write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let mut suspended = config("    npm: {}\n");
        suspended
            .release_units
            .get_mut("component")
            .unwrap()
            .disposition = ReleaseUnitDisposition::Suspended;
        assert!(
            select_publications(workspace.root(), &suspended)
                .expect("suspended selection runs")
                .is_empty(),
            "a release unit that does not release cannot publish"
        );

        let managed = config("    npm: {}\n");
        assert_eq!(
            select_publications(workspace.root(), &managed)
                .expect("managed selection runs")
                .len(),
            1
        );
    }

    #[test]
    fn selects_rust_homebrew_and_reports_ambiguous_combinations() {
        let workspace = Workspace::new("rust-homebrew-selection");
        workspace.write("component/Cargo.toml", "[package]\nname = \"component\"\n");
        let selected = select_publications(
            workspace.root(),
            &config("    homebrew: { repository: example-org/homebrew-tap }\n"),
        )
        .expect("Rust package selects the maintained Homebrew route");
        assert_eq!(selected[0].capability, Capability::RustCrate);
        assert_eq!(selected[0].packager, Packager::CargoArchive);

        let workspace = Workspace::new("ambiguous");
        workspace
            .write("component/Dockerfile", "FROM scratch\n")
            .write(
                "component/devcontainer-feature.json",
                r#"{"id":"example","version":"1.0.0"}"#,
            );
        let error = select_publications(workspace.root(), &config("    oci:\n      ghcr: {}\n"))
            .expect_err("ambiguous combination rejected");
        assert!(
            error
                .to_string()
                .contains("matches 2 publishable detector candidates")
                && error.to_string().contains("component/Dockerfile")
                && error
                    .to_string()
                    .contains("component/devcontainer-feature.json"),
            "{error}"
        );
    }

    #[test]
    fn refuses_ambiguity_before_filtering_candidates_by_publisher() {
        let workspace = Workspace::new("cross-publisher-ambiguity");
        workspace
            .write(
                "component/package.json",
                r#"{"name":"example-package","version":"1.0.0"}"#,
            )
            .write("component/Dockerfile", "FROM scratch\n");
        let error = select_publications(workspace.root(), &config("    npm: {}\n"))
            .expect_err("one declaration cannot silently choose its publisher's artifact");
        let message = error.to_string();
        assert!(
            message.contains("matches 2 publishable detector candidates"),
            "{message}"
        );
        assert!(message.contains("component/package.json"), "{message}");
        assert!(message.contains("component/Dockerfile"), "{message}");
    }

    #[test]
    fn reports_every_refusing_package_in_one_run() {
        let workspace = Workspace::new("multiple-refusing-packages");
        let text = GITHUB.replace(
            "    path: component\n",
            "    path: component\n    packages:\n      first:\n        path: first\n        npm: {}\n      second:\n        path: second\n        cargo: {}\n",
        );
        let config = Config::from_yaml(&text).expect("package declarations");
        let selection = resolve_publications(workspace.root(), &config).expect("selection runs");
        assert_eq!(
            selection.diagnostics.len(),
            2,
            "{:?}",
            selection.diagnostics
        );
        assert!(selection
            .diagnostics
            .iter()
            .any(|message| message.contains("package first")));
        assert!(selection
            .diagnostics
            .iter()
            .any(|message| message.contains("package second")));
    }

    #[test]
    fn root_scoped_packages_match_candidates_below_the_workspace_root() {
        let workspace = Workspace::new("root-scoped-package");
        workspace.write(
            "nested/package.json",
            r#"{"name":"example-package","version":"1.0.0"}"#,
        );
        let text = GITHUB
            .replace("    path: component\n", "    path: .\n")
            .replace("        path: component\n", "        path: .\n");
        let config = Config::from_yaml(&text).expect("root release unit");
        let derived = derive_capabilities(workspace.root(), &config, "component")
            .expect("root package derives nested evidence");
        assert_eq!(
            capability_set(&derived),
            BTreeSet::from([Capability::NodePackage])
        );
    }

    #[test]
    fn unknown_release_unit_is_a_validation_error() {
        let workspace = Workspace::new("unknown-release-unit");
        let error = derive_capabilities(workspace.root(), &config(""), "absent")
            .expect_err("unknown identities are refused without a panic");
        assert!(
            error
                .to_string()
                .contains("release unit absent is not configured"),
            "{error}"
        );
    }

    #[test]
    fn ambiguous_refusal_enumerates_only_live_candidates() {
        let workspace = Workspace::new("excluded-ambiguous-candidate");
        workspace
            .write("component/Dockerfile", "FROM scratch\n")
            .write(
                "component/devcontainer-feature.json",
                r#"{"id":"example","version":"1.0.0"}"#,
            );
        let mut config = config("    oci:\n      ghcr: {}\n");
        let ambiguity = select_publications(workspace.root(), &config)
            .expect_err("the fixture reaches ambiguity before exclusion");
        assert!(
            ambiguity
                .to_string()
                .contains("matches 2 publishable detector candidates"),
            "{ambiguity}"
        );
        let excluded = detector_candidates(workspace.root())
            .expect("detector candidates")
            .into_iter()
            .find(|candidate| candidate.detector == "devcontainer-feature")
            .expect("feature candidate");
        let evidence_digest = excluded
            .evidence
            .iter()
            .find(|evidence| evidence.path == excluded.path)
            .expect("exact-path evidence")
            .digest
            .clone();
        config
            .discovery
            .excluded_paths
            .push(crate::config::ExcludedPathReceipt {
                detector: excluded.detector,
                path: excluded.path,
                evidence_digest,
            });

        let selected = select_publications(workspace.root(), &config)
            .expect("one live image candidate resolves");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].capability, Capability::RunnableImage);

        workspace.write(
            "component/devcontainer-feature.json",
            r#"{"id":"example","version":"2.0.0"}"#,
        );
        let stale = select_publications(workspace.root(), &config)
            .expect_err("changed evidence reopens the excluded candidate");
        assert!(
            stale
                .to_string()
                .contains("matches 2 publishable detector candidates"),
            "{stale}"
        );
    }

    #[test]
    fn managed_receipt_selects_one_candidate_at_a_coincident_path() {
        let workspace = Workspace::new("managed-package-receipt");
        workspace
            .write(
                "component/package.json",
                r#"{"name":"example-package","version":"1.0.0"}"#,
            )
            .write("component/Dockerfile", "FROM scratch\n");
        let mut config = config("");
        config
            .discovery
            .managed_paths
            .push(crate::config::ManagedPathReceipt {
                detector: "npm-package".to_owned(),
                path: PathBuf::from("component/package.json"),
                release_unit: "component".to_owned(),
                package: "package".to_owned(),
            });

        let derived = derive_component(workspace.root(), &config).expect("capability derives");
        assert_eq!(
            capability_set(&derived),
            BTreeSet::from([Capability::NodePackage])
        );
    }

    #[test]
    fn one_publisher_is_reached_through_multiple_package_capabilities() {
        let workspace = Workspace::new("multiple-oci-package-routes");
        workspace
            .write("component/image/Dockerfile", "FROM scratch\n")
            .write(
                "component/feature/devcontainer-feature.json",
                r#"{"id":"example","version":"1.0.0"}"#,
            );
        let text = GITHUB.replace(
            "    path: component\n",
            "    path: component\n    packages:\n      feature:\n        path: feature\n        oci: { ghcr: {} }\n      image:\n        path: image\n        oci: { ghcr: {} }\n",
        );
        let config = Config::from_yaml(&text).expect("package declarations");

        let selected = select_publications(workspace.root(), &config)
            .expect("each package selects its own route");
        assert_eq!(
            selected
                .iter()
                .map(|publication| publication.packager)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([Packager::Buildx, Packager::DevContainerCli])
        );
        assert_eq!(
            selected
                .iter()
                .map(SelectedPublication::identity)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "component/feature/oci/ghcr".to_owned(),
                "component/image/oci/ghcr".to_owned(),
            ]),
            "the package segment distinguishes two publications sharing the other three segments"
        );
    }

    #[test]
    fn maintained_catalog_selects_unambiguously() {
        // Selection matches on capability, publisher, and target only, so two
        // entries sharing those three would make every such publication fail as
        // ambiguous no matter which packagers they name.
        let mut index = BTreeSet::new();
        for recipe in catalog() {
            assert!(
                index.insert((recipe.capability, recipe.publisher, recipe.target)),
                "catalog repeats the index selection depends on: {}/{}/{}",
                recipe.capability,
                recipe.publisher,
                recipe.target
            );
        }
    }

    /// Reads a probe can survive: the file is gone by the time it is read.
    ///
    /// A path that does not exist when the read reaches it produces exactly the
    /// error a file removed inside the check-to-read window produces, so this
    /// is the racing outcome injected directly rather than waited for.
    #[test]
    fn a_probed_file_taken_before_the_read_is_absent_rather_than_an_error() {
        let workspace = Workspace::new("probe-taken");
        let taken = workspace.root().join("already-gone.toml");
        assert_eq!(
            probed_file_text(&taken).expect("a file that has gone is absent, not a failure"),
            None
        );
    }

    /// Reads a probe must not swallow: the path is there and unreadable.
    #[test]
    fn a_probed_file_that_cannot_be_read_is_still_reported() {
        let workspace = Workspace::new("probe-unreadable");
        let directory = workspace.root().join("Cargo.toml");
        std::fs::create_dir(&directory).expect("create directory in the manifest position");
        assert!(
            probed_file_text(&directory).is_err(),
            "only a file that has gone is absent; every other read failure is reported"
        );
    }

    #[test]
    fn a_probed_file_hook_is_cleared_when_its_callback_panics() {
        let workspace = Workspace::new("probe-hook-panic");
        let path = workspace.root().join("input.txt");
        std::fs::write(&path, "sample").expect("write the probed input");
        let _hook = ProbedFileBeforeReadHook::install(|| panic!("forced hook failure"));

        let panic = std::panic::catch_unwind(|| probed_file_text(&path));

        assert!(panic.is_err(), "the hook callback must have run");
        PROBED_FILE_BEFORE_READ.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "a panicking hook callback must not remain installed"
            );
        });
    }

    #[test]
    fn a_probed_file_hook_is_cleared_when_its_scope_panics_before_the_read() {
        let panic = std::panic::catch_unwind(|| {
            let _hook = ProbedFileBeforeReadHook::install(|| {});
            panic!("forced probe failure");
        });

        assert!(panic.is_err(), "the probe scope must have unwound");
        PROBED_FILE_BEFORE_READ.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "an uncalled hook must be cleared while its scope unwinds"
            );
        });
    }

    /// Run one raced probe after proving it can see a held publication.
    ///
    /// The probing thread commands each publication and withdrawal. The raced
    /// probe passes its existence check while the file is there, then reads
    /// after the competitor has removed it and blocked. This is the
    /// interleaving a concurrently dropped fixture workspace produces. An
    /// error or a raced answer that still reports the file present is a defect.
    ///
    /// Absence is also what a competitor that never publishes produces. The
    /// publication rendezvous distinguishes the two: before racing, the probe
    /// has to observe a publication while the competitor holds it in place.
    /// The raced probe then starts from another confirmed publication. A
    /// one-shot test hook commands withdrawal after the existence check enters
    /// `probed_file_text`, waits until removal completes, and clears itself on
    /// unwind. The competitor accepts no further command until the raced read
    /// returns, and the raced answer is asserted absent.
    fn under_removal<T>(
        label: &str,
        file: &str,
        probe: impl Fn(&Path, &Path) -> Result<T>,
        present: impl Fn(&T) -> bool,
        contents: &str,
    ) {
        use std::sync::mpsc;

        #[derive(Clone, Copy)]
        enum CompetitorCommand {
            Publish,
            Withdraw,
            Stop,
        }

        let workspace = Workspace::new(label);
        let root = workspace.root().to_path_buf();
        let path = root.join(file);
        let staged = root.join("staged-contents");
        std::fs::write(&staged, contents).expect("stage the contents the competitor publishes");
        let (published, publication) = mpsc::channel();
        let (withdrawn, withdrawal) = mpsc::channel();
        let (command, commands) = mpsc::channel();
        let competitor = {
            let staged = staged.clone();
            std::thread::spawn(move || {
                loop {
                    match commands
                        .recv()
                        .expect("the probe commands each competitor transition")
                    {
                        CompetitorCommand::Publish => {
                            // Published by rename and withdrawn by removal, so
                            // the probe sees the file whole or not at all. This
                            // harness opens the removal window, not a torn read.
                            let scratch = path.with_extension("staging");
                            std::fs::copy(&staged, &scratch).expect("stage the next publication");
                            std::fs::rename(&scratch, &path).expect("publish the probed file");
                            published
                                .send(())
                                .expect("the probe waits for each publication");
                        }
                        CompetitorCommand::Withdraw => {
                            std::fs::remove_file(&path).expect("withdraw the probed file");
                            withdrawn
                                .send(())
                                .expect("the probe waits for each withdrawal");
                        }
                        CompetitorCommand::Stop => {
                            break;
                        }
                    }
                }
            })
        };

        let relative = Path::new(file);
        command
            .send(CompetitorCommand::Publish)
            .expect("command the positive-control publication");
        publication
            .recv()
            .expect("the competitor publishes the positive control");
        let control = probe(&root, relative);
        let control_was_present = control.as_ref().is_ok_and(&present);
        command
            .send(CompetitorCommand::Withdraw)
            .expect("release the positive-control publication");
        withdrawal
            .recv()
            .expect("the competitor withdraws the positive control");

        let raced = control_was_present.then(|| {
            let window_command = command.clone();
            let _hook = ProbedFileBeforeReadHook::install(move || {
                window_command
                    .send(CompetitorCommand::Withdraw)
                    .expect("release the withdrawal inside the probe window");
                withdrawal
                    .recv()
                    .expect("the file is withdrawn before the probe reads it");
            });
            command
                .send(CompetitorCommand::Publish)
                .expect("command the raced publication");
            publication
                .recv()
                .expect("the competitor publishes before the raced probe");
            probe(&root, relative)
        });

        command
            .send(CompetitorCommand::Stop)
            .expect("stop the competitor after its final publication");
        competitor.join().expect("the competing thread finishes");

        if !control_was_present {
            match control {
                Ok(_) => panic!(
                    "the probe did not observe the competitor's held publication of {file}, so the removal race was not started"
                ),
                Err(error) => panic!(
                    "the probe failed while the competitor held {file} published as a positive control: {error:?}"
                ),
            }
        }
        if let Some(raced) = raced {
            match raced {
                Err(error) => {
                    panic!("{file} was removed while the probe ran and became a failure: {error:?}")
                }
                Ok(answer) => assert!(
                    !present(&answer),
                    "the raced probe still saw {file} present after its forced withdrawal"
                ),
            }
        }
    }
}
