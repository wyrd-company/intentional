// ---
// relationships:
//   implements: github-release-executor
// ---

//! Release-unit capability derivation and maintained publication recipe selection.

use crate::config::{Config, ReleaseUnitConfig};
use crate::error::{Error, Result};
use crate::evidence::assemble::CleanClientMode;
use crate::init::{evidence, SourceEvidence};
use crate::model::{AttachedComponent, PublisherKind, ReleaseUnitDisposition};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

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
            Self::GoReleaser => "goreleaser",
            Self::Buildx => "buildx",
            Self::DevContainerCli => "devcontainer-cli",
        }
    }

    /// Release-unit-relative paths that satisfy this packager's native configuration.
    pub const fn configuration_paths(self) -> &'static [&'static str] {
        match self {
            Self::Npm => &["package.json"],
            Self::Cargo => &["Cargo.toml"],
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
        retrieval: CleanClientMode::AuthenticatedDraft,
    },
    Recipe {
        capability: Capability::GoApplication,
        packager: Packager::GoReleaser,
        publisher: PublisherKind::Apt,
        target: PRIMARY_TARGET,
        components: &[],
        retrieval: CleanClientMode::AuthenticatedDraft,
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
    /// Derived capability.
    pub capability: Capability,
    /// Native artifact proving the capability.
    pub evidence: SourceEvidence,
}

/// One configured publication resolved to exactly one maintained recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPublication {
    /// Configured release unit.
    pub release_unit: String,
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
}

impl SelectedPublication {
    /// Stable identity used by diagnostics and evidence.
    pub fn identity(&self) -> String {
        format!("{}/{}/{}", self.release_unit, self.publisher, self.target)
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

/// Resolve every configured publication, collecting each failure instead of
/// stopping at the first, so one run reports every unresolved target.
pub fn resolve_publications(root: &Path, config: &Config) -> Result<PublicationSelection> {
    let mut selection = PublicationSelection::default();
    for (id, release_unit) in &config.release_units {
        // A suspended release unit does not release, so it cannot publish.
        if release_unit.disposition != ReleaseUnitDisposition::Managed
            || release_unit.publishers().is_empty()
        {
            continue;
        }
        let capabilities = match derive_capabilities(root, release_unit) {
            Ok(derived) => capability_set(&derived),
            Err(Error::Validation(message)) => {
                selection.diagnostics.push(message);
                continue;
            }
            Err(error) => return Err(error),
        };
        for (publisher, target, configured) in configured_targets(release_unit) {
            match select_one(
                root,
                id,
                release_unit,
                &capabilities,
                publisher,
                target,
                &configured,
            ) {
                Ok(publication) => selection.selected.push(publication),
                Err(Error::Validation(message)) => selection.diagnostics.push(message),
                Err(error) => return Err(error),
            }
        }
    }
    Ok(selection)
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
fn configured_targets(
    release_unit: &ReleaseUnitConfig,
) -> Vec<(PublisherKind, String, Configured)> {
    let mut targets = Vec::new();
    if let Some(npm) = &release_unit.npm {
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
    if release_unit.cargo.is_some() {
        targets.push((
            PublisherKind::Cargo,
            PRIMARY_TARGET.to_owned(),
            Configured::default(),
        ));
    }
    if let Some(homebrew) = &release_unit.homebrew {
        targets.push((
            PublisherKind::Homebrew,
            PRIMARY_TARGET.to_owned(),
            Configured {
                destination: Some(homebrew.repository.clone()),
                ..Configured::default()
            },
        ));
    }
    for (publisher, present) in [
        (PublisherKind::Rpm, release_unit.rpm.is_some()),
        (PublisherKind::Apt, release_unit.apt.is_some()),
        (PublisherKind::Aur, release_unit.aur.is_some()),
    ] {
        if present {
            targets.push((publisher, PRIMARY_TARGET.to_owned(), Configured::default()));
        }
    }
    if let Some(oci) = &release_unit.oci {
        if let Some(dockerhub) = &oci.dockerhub {
            targets.push((
                PublisherKind::Oci,
                "dockerhub".to_owned(),
                Configured {
                    destination: dockerhub.repository.clone(),
                    omit: dockerhub.omit.clone(),
                    destination_required: true,
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
                },
            ));
        }
    }
    targets
}

#[derive(Debug, Default, Clone)]
struct Configured {
    destination: Option<String>,
    omit: Vec<AttachedComponent>,
    destination_required: bool,
}

fn select_one(
    root: &Path,
    id: &str,
    release_unit: &ReleaseUnitConfig,
    capabilities: &BTreeSet<Capability>,
    publisher: PublisherKind,
    target: String,
    configured: &Configured,
) -> Result<SelectedPublication> {
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
                    "no maintained publication recipe matches the configured target {id}/{publisher}/{target}: {reason}"
                )));
            }
            return Err(Error::Validation(format!(
                "no maintained publication recipe matches the configured target {id}/{publisher}/{target}; derived capabilities are {}",
                capability_names(capabilities)
            )))
        }
        many => {
            return Err(Error::Validation(format!(
                "configured target {id}/{publisher}/{target} matches {} maintained recipes ({}); the release unit must derive one publishable capability",
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
                "omitted component {component} is not supported by the target recipe for {id}/{publisher}/{target}"
            )));
        }
    }
    let destination = match (&configured.destination, publisher) {
        (Some(destination), _) => Some(destination.clone()),
        (None, PublisherKind::Cargo) => Some(cargo_registry(root, release_unit)?),
        (None, _) if configured.destination_required => {
            return Err(Error::Validation(format!(
                "configured target {id}/{publisher}/{target} requires an explicit repository; its destination identity is not derivable"
            )))
        }
        (None, _) => None,
    };
    let retrieval = retrieval_mode(&recipe, destination.as_deref());
    Ok(SelectedPublication {
        release_unit: id.to_owned(),
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
    if main_package_directory(&directory)?.is_some() {
        return Ok(None);
    }
    Ok(Some(format!(
        "release unit {} is a Go module but declares no discoverable main package, so it derives no {} capability; the maintained recipes build a command, and the packager finds one in the release-unit root, under cmd to {COMMAND_SEARCH_DEPTH} directories deep, or wherever the native GoReleaser configuration's builds[].main names",
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

/// Derive every publishable capability of one release unit from its native evidence.
pub fn derive_capabilities(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
) -> Result<Vec<CapabilityEvidence>> {
    let mut derived = Vec::new();
    let unit = &release_unit.path;
    if node_package_is_publishable(root, &unit.join("package.json"))? {
        derived.push(capability_evidence(
            root,
            Capability::NodePackage,
            unit.join("package.json"),
        )?);
    }
    if cargo_manifest(root, &unit.join("Cargo.toml"))?
        .is_some_and(|manifest| manifest.publishable())
    {
        derived.push(capability_evidence(
            root,
            Capability::RustCrate,
            unit.join("Cargo.toml"),
        )?);
    }
    if root.join(unit).join("go.mod").is_file()
        && main_package_directory(&root.join(unit))?.is_some()
    {
        derived.push(capability_evidence(
            root,
            Capability::GoApplication,
            unit.join("go.mod"),
        )?);
    }
    if root.join(unit).join("Dockerfile").is_file() {
        derived.push(capability_evidence(
            root,
            Capability::RunnableImage,
            unit.join("Dockerfile"),
        )?);
    }
    if root.join(unit).join("devcontainer-feature.json").is_file() {
        derived.push(capability_evidence(
            root,
            Capability::DevContainerFeature,
            unit.join("devcontainer-feature.json"),
        )?);
    }
    Ok(derived)
}

fn capability_evidence(
    root: &Path,
    capability: Capability,
    relative: PathBuf,
) -> Result<CapabilityEvidence> {
    Ok(CapabilityEvidence {
        capability,
        evidence: evidence(root, &relative, Vec::new())?,
    })
}

/// Default Cargo registry when a manifest states no explicit destination.
const CRATES_IO: &str = "crates.io";

fn node_package_is_publishable(root: &Path, relative: &Path) -> Result<bool> {
    let path = root.join(relative);
    if !path.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
    let value = serde_json::from_str::<serde_json::Value>(&text).map_err(|error| {
        Error::Validation(format!("{} is not valid JSON: {error}", relative.display()))
    })?;
    Ok(value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && value.get("private").and_then(serde_json::Value::as_bool) != Some(true))
}

/// Publication-relevant contents of one Cargo package manifest.
struct CargoManifest {
    /// Registries named by `package.publish`, empty when it selects the default.
    registries: Vec<String>,
    /// Whether `package.publish` permits publication at all.
    permitted: bool,
}

impl CargoManifest {
    fn publishable(&self) -> bool {
        self.permitted
    }
}

/// Read one Cargo package manifest, or `None` when it declares no package.
fn cargo_manifest(root: &Path, relative: &Path) -> Result<Option<CargoManifest>> {
    let path = root.join(relative);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
    let document = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
        Error::Validation(format!("{} is not valid TOML: {error}", relative.display()))
    })?;
    let Some(package) = document.get("package") else {
        return Ok(None);
    };
    if package.get("name").is_none() {
        return Ok(None);
    }
    // Cargo's `publish` accepts false, true, and a registry array. false and an
    // empty array both mean the crate must never be published.
    let publish = package.get("publish");
    let registries = publish
        .and_then(toml_edit::Item::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let permitted = match publish {
        None => true,
        Some(item) => match item.as_bool() {
            Some(permitted) => permitted,
            None => !registries.is_empty(),
        },
    };
    Ok(Some(CargoManifest {
        registries,
        permitted,
    }))
}

fn cargo_registry(root: &Path, release_unit: &ReleaseUnitConfig) -> Result<String> {
    let relative = release_unit.path.join("Cargo.toml");
    let Some(manifest) = cargo_manifest(root, &relative)? else {
        return Ok(CRATES_IO.to_owned());
    };
    match manifest.registries.as_slice() {
        [] => Ok(CRATES_IO.to_owned()),
        [registry] => Ok(registry.clone()),
        many => Err(Error::Validation(format!(
            "release unit {} publishes to {} Cargo registries ({}); Cargo publication requires exactly one primary destination",
            release_unit.path.display(),
            many.len(),
            many.join(", ")
        ))),
    }
}

/// Directories a Go module's command package can be discovered in.
///
/// Real Go repositories place the command a release publishes in one of three
/// shapes, and the derivation reads all three because the packager can only
/// build what one of them names.
///
/// The packager's own configuration is the first and most authoritative source:
/// GoReleaser's `builds[].main` states the main package directory outright, so
/// a repository whose command lives somewhere idiosyncratic has already said
/// where. The conventional `cmd/<name>` layout is the second, read to a bounded
/// depth because `cmd/<name>/<platform>` and `cmd/<name>/internal` both occur.
/// The module root is the third, which is the whole layout of a single-binary
/// tool.
///
/// Depth is bounded rather than unbounded on purpose. An unbounded walk of a
/// release unit would read `vendor/`, `testdata/`, and every dependency copied
/// into the tree, and would derive a publishable capability from a `package
/// main` that belongs to something the release does not publish.
const COMMAND_SEARCH_DEPTH: usize = 3;

/// Where one release unit's Go command package lives, when it can be found.
///
/// Discovery returns the directory rather than a boolean because the reason a
/// capability was withheld has to be reportable: a `go.mod` with no discoverable
/// command is a diagnosable configuration, not an absent Go module.
fn main_package_directory(directory: &Path) -> Result<Option<PathBuf>> {
    for root in command_search_roots(directory)? {
        if declares_main_package(&root)? {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

/// Every directory the main package could be discovered in, in priority order.
fn command_search_roots(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = crate::executor::goreleaser::read(directory)?
        .map(|config| {
            config
                .main_directories
                .into_iter()
                .map(|relative| directory.join(relative))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    roots.push(directory.to_owned());
    collect_command_directories(&directory.join("cmd"), COMMAND_SEARCH_DEPTH, &mut roots)?;
    let mut seen = BTreeSet::new();
    roots.retain(|root| root.is_dir() && seen.insert(root.clone()));
    Ok(roots)
}

/// Collect `cmd` subdirectories to a bounded depth.
fn collect_command_directories(
    directory: &Path,
    remaining: usize,
    collected: &mut Vec<PathBuf>,
) -> Result<()> {
    if remaining == 0 || !directory.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(directory).map_err(|error| Error::io(directory, error))?;
    let mut children = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| Error::io(directory, error))?.path();
        if path.is_dir() {
            children.push(path);
        }
    }
    children.sort();
    for child in children {
        collected.push(child.clone());
        collect_command_directories(&child, remaining - 1, collected)?;
    }
    Ok(())
}

/// Whether the Go files directly inside one directory declare `package main`.
///
/// The clause is read as Go declares it rather than as an exact line, because a
/// build-constrained file and a file carrying an import comment both spell the
/// package clause with something after it and both are ordinary Go.
fn declares_main_package(directory: &Path) -> Result<bool> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(false);
    };
    for entry in entries {
        let path = entry.map_err(|error| Error::io(directory, error))?.path();
        if path.extension().is_none_or(|extension| extension != "go") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.lines().any(is_main_package_clause) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether one source line is the `package main` clause.
fn is_main_package_clause(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("package ") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix("main") else {
        return false;
    };
    let rest = rest.trim_start_matches(';').trim();
    rest.is_empty() || rest.starts_with("//") || rest.starts_with("/*")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;

    const GITHUB: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
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
        let text = GITHUB.replace(
            "    path: component\n",
            &format!("    path: component\n{publisher}"),
        );
        Config::from_yaml(&text).expect("fixture config")
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
        let derived = derive_capabilities(workspace.root(), &config.release_units["component"])
            .expect("capabilities derive");
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
    fn treats_private_and_virtual_manifests_as_unpublishable() {
        let workspace = Workspace::new("unpublishable");
        workspace
            .write(
                "component/package.json",
                r#"{"name":"example-component","private":true}"#,
            )
            .write("component/Cargo.toml", "[workspace]\nmembers = []\n");
        let derived = derive_capabilities(workspace.root(), &config("").release_units["component"])
            .expect("capabilities derive");
        assert!(capability_set(&derived).is_empty());
    }

    #[test]
    fn honors_cargo_publish_restrictions() {
        let workspace = Workspace::new("cargo-publish");
        for restriction in ["publish = false", "publish = []"] {
            workspace.write(
                "component/Cargo.toml",
                &format!("[package]\nname = \"component\"\n{restriction}\n"),
            );
            let derived =
                derive_capabilities(workspace.root(), &config("").release_units["component"])
                    .expect("capabilities derive");
            assert!(
                capability_set(&derived).is_empty(),
                "{restriction} withholds the rust-crate capability"
            );
            let error = select_publications(workspace.root(), &config("    cargo: {}\n"))
                .expect_err("unpublishable crate rejected");
            assert!(
                error
                    .to_string()
                    .contains("no maintained publication recipe matches the configured target"),
                "{error}"
            );
        }

        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"component\"\npublish = true\n",
        );
        let selected = select_publications(workspace.root(), &config("    cargo: {}\n"))
            .expect("publishable crate selects");
        assert_eq!(selected[0].destination.as_deref(), Some("crates.io"));
    }

    #[test]
    fn reports_manifests_that_cannot_be_parsed() {
        let workspace = Workspace::new("malformed");
        workspace.write("component/package.json", "{ not json");
        let error = derive_capabilities(workspace.root(), &config("").release_units["component"])
            .expect_err("malformed manifest reported");
        assert!(
            error.to_string().contains("package.json is not valid JSON"),
            "{error}"
        );

        let workspace = Workspace::new("malformed-toml");
        workspace.write("component/Cargo.toml", "[package\nname =");
        let error = derive_capabilities(workspace.root(), &config("").release_units["component"])
            .expect_err("malformed manifest reported");
        assert!(
            error.to_string().contains("Cargo.toml is not valid TOML"),
            "{error}"
        );
    }

    #[test]
    fn selects_one_recipe_for_each_configured_target() {
        let workspace = Workspace::new("selection");
        workspace
            .write(
                "component/package.json",
                r#"{"name":"example-component","version":"1.0.0"}"#,
            )
            .write("component/Dockerfile", "FROM scratch\n");
        let selected = select_publications(
            workspace.root(),
            &config("    npm: { additional-targets: { github: {} } }\n    oci:\n      ghcr: { omit: [ signature ] }\n"),
        )
        .expect("publications select");
        let identities = selected
            .iter()
            .map(SelectedPublication::identity)
            .collect::<Vec<_>>();
        assert_eq!(
            identities,
            vec![
                "component/npm/primary".to_owned(),
                "component/npm/github".to_owned(),
                "component/oci/ghcr".to_owned(),
            ]
        );
        assert_eq!(selected[0].destination.as_deref(), Some("npmjs"));
        assert_eq!(selected[0].packager, Packager::Npm);
        assert_eq!(selected[1].destination, None);
        assert_eq!(selected[2].capability, Capability::RunnableImage);
        assert_eq!(
            selected[2].components,
            vec![AttachedComponent::Sbom, AttachedComponent::Provenance]
        );
    }

    #[test]
    fn withholds_publication_from_suspended_release_units() {
        let workspace = Workspace::new("suspended");
        workspace.write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let suspended = config("    disposition: suspended\n    npm: {}\n");
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
    fn reports_unsupported_and_ambiguous_combinations() {
        let workspace = Workspace::new("unsupported");
        workspace.write("component/Cargo.toml", "[package]\nname = \"component\"\n");
        let error = select_publications(
            workspace.root(),
            &config("    homebrew: { repository: example-org/homebrew-tap }\n"),
        )
        .expect_err("unsupported combination rejected");
        assert!(
            error
                .to_string()
                .contains("no maintained publication recipe matches the configured target"),
            "{error}"
        );

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
            error.to_string().contains("matches 2 maintained recipes"),
            "{error}"
        );
    }

    #[test]
    fn rejects_omitted_components_absent_from_the_target_recipe() {
        let workspace = Workspace::new("omit");
        workspace.write(
            "component/devcontainer-feature.json",
            r#"{"id":"example","version":"1.0.0"}"#,
        );
        let error = select_publications(
            workspace.root(),
            &config("    oci:\n      ghcr: { omit: [ sbom ] }\n"),
        )
        .expect_err("unsupported omission rejected");
        assert!(
            error
                .to_string()
                .contains("omitted component sbom is not supported by the target recipe"),
            "{error}"
        );
    }

    #[test]
    fn resolves_the_concrete_cargo_primary_destination() {
        let workspace = Workspace::new("cargo");
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"component\"\nversion = \"1.0.0\"\n",
        );
        let selected = select_publications(workspace.root(), &config("    cargo: {}\n"))
            .expect("cargo publication selects");
        assert_eq!(selected[0].destination.as_deref(), Some("crates.io"));

        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"component\"\npublish = [\"example-registry\"]\n",
        );
        let selected = select_publications(workspace.root(), &config("    cargo: {}\n"))
            .expect("configured registry selects");
        assert_eq!(selected[0].destination.as_deref(), Some("example-registry"));
    }

    #[test]
    fn requires_an_explicit_docker_hub_repository() {
        let workspace = Workspace::new("dockerhub");
        workspace.write("component/Dockerfile", "FROM scratch\n");
        let error =
            select_publications(workspace.root(), &config("    oci:\n      dockerhub: {}\n"))
                .expect_err("underivable destination rejected");
        assert!(
            error
                .to_string()
                .contains("requires an explicit repository"),
            "{error}"
        );

        let selected = select_publications(
            workspace.root(),
            &config("    oci:\n      dockerhub: { repository: example-org/example-image }\n"),
        )
        .expect("configured repository selects");
        assert_eq!(
            selected[0].destination.as_deref(),
            Some("example-org/example-image")
        );
    }

    // Two tables now state which destinations resolve a GitHub Release asset:
    // the catalog's retrieval mode and the draft handoff's publisher list. The
    // handoff builds an asset inventory for the publishers in its list, and the
    // recipe that consumes one is the recipe whose retrieval is
    // authenticated-draft, so a destination in one table and not the other is
    // either handed assets no recipe retrieves or asked for a retrieval no
    // handoff supplies.
    #[test]
    fn agrees_with_the_draft_handoff_about_which_destinations_read_a_draft_asset() {
        for recipe in catalog() {
            assert_eq!(
                recipe.retrieval == CleanClientMode::AuthenticatedDraft,
                crate::publication::draft::is_draft_dependent(recipe.publisher),
                "{}/{} disagrees with the draft handoff about draft-asset retrieval",
                recipe.publisher,
                recipe.target
            );
        }
    }

    /// The Go layouts a maintained GoReleaser recipe has to recognize.
    ///
    /// Each entry is the layout of a real Go repository shape, paired with the
    /// files that make it that shape. Deriving the capability from one layout
    /// and not another would leave a publishable repository reporting that no
    /// maintained recipe matches its configured target.
    const GO_LAYOUTS: [(&str, &[(&str, &str)]); 5] = [
        (
            "a single-binary tool with main at the module root",
            &[("component/main.go", "package main\n\nfunc main() {}\n")],
        ),
        (
            "the conventional cmd/<name> layout",
            &[(
                "component/cmd/example/main.go",
                "package main\n\nfunc main() {}\n",
            )],
        ),
        (
            "a command nested below cmd/<name>",
            &[(
                "component/cmd/example/app/main.go",
                "package main\n\nfunc main() {}\n",
            )],
        ),
        (
            "a main package the native GoReleaser configuration names",
            &[
                (
                    "component/.goreleaser.yaml",
                    "version: 2\nbuilds:\n  - main: ./tools/example\n",
                ),
                (
                    "component/tools/example/main.go",
                    "package main\n\nfunc main() {}\n",
                ),
            ],
        ),
        (
            "a package clause carrying an import comment",
            &[(
                "component/main.go",
                "package main // import \"example.test/component\"\n\nfunc main() {}\n",
            )],
        ),
    ];

    #[test]
    fn derives_the_go_application_capability_from_every_supported_layout() {
        for (layout, files) in GO_LAYOUTS {
            let workspace = Workspace::new("go-layout");
            workspace.write("component/go.mod", "module example.test/component\n");
            for (relative, contents) in files {
                workspace.write(relative, contents);
            }
            let derived =
                derive_capabilities(workspace.root(), &config("").release_units["component"])
                    .expect("capabilities derive");
            assert!(
                capability_set(&derived).contains(&Capability::GoApplication),
                "{layout} derives the go-application capability"
            );

            let selected = select_publications(
                workspace.root(),
                &config("    homebrew: { repository: example-org/homebrew-tap }\n"),
            )
            .unwrap_or_else(|error| panic!("{layout} selects a recipe: {error}"));
            assert_eq!(selected[0].packager, Packager::GoReleaser);
        }
    }

    #[test]
    fn reports_a_go_module_with_no_discoverable_command_as_the_cause() {
        let workspace = Workspace::new("go-no-command");
        workspace
            .write("component/go.mod", "module example.test/component\n")
            .write(
                "component/library.go",
                "package component\n\nfunc Example() {}\n",
            );
        let error = select_publications(
            workspace.root(),
            &config("    homebrew: { repository: example-org/homebrew-tap }\n"),
        )
        .expect_err("a module with no command is refused");
        assert!(
            error
                .to_string()
                .contains("is a Go module but declares no discoverable main package"),
            "{error}"
        );

        // The diagnostic names the missing command rather than the derived set,
        // which is the whole point: "derived capabilities are none" tells a Go
        // repository nothing about what it has to fix.
        assert!(
            !error.to_string().contains("derived capabilities are"),
            "{error}"
        );
    }

    #[test]
    fn withholds_the_capability_from_a_main_package_outside_the_release_unit() {
        // An unbounded search would reach a vendored or copied command and
        // derive a capability for something the release does not publish.
        let workspace = Workspace::new("go-vendored");
        workspace
            .write("component/go.mod", "module example.test/component\n")
            .write(
                "component/vendor/example.test/other/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write(
                "component/cmd/example/deep/deeper/deepest/main.go",
                "package main\n\nfunc main() {}\n",
            );
        let derived = derive_capabilities(workspace.root(), &config("").release_units["component"])
            .expect("capabilities derive");
        assert!(
            !capability_set(&derived).contains(&Capability::GoApplication),
            "a command outside the searched layout derives nothing"
        );
    }

    #[test]
    fn a_go_module_that_derives_its_capability_reports_no_withheld_reason() {
        // The reason exists to explain an absent capability. Reporting one for
        // a release unit whose capability derived would make an unrelated
        // unsupported combination read as a Go layout problem.
        let workspace = Workspace::new("go-unrelated");
        workspace
            .write("component/go.mod", "module example.test/component\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n");
        let error = select_publications(workspace.root(), &config("    npm: {}\n"))
            .expect_err("an unsupported combination is refused");
        assert!(
            error.to_string().contains("derived capabilities are"),
            "{error}"
        );
        assert!(
            !error.to_string().contains("no discoverable main package"),
            "{error}"
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
}
