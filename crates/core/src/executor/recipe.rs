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
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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

/// Resolve every configured publication, collecting each failure instead of
/// stopping at the first, so one run reports every unresolved target.
pub fn resolve_publications(root: &Path, config: &Config) -> Result<PublicationSelection> {
    let mut selection = PublicationSelection::default();
    for (id, release_unit) in &config.release_units {
        if !selects_publications(release_unit) {
            continue;
        }
        if let Err(Error::Validation(message)) = validate_package_artifacts(root, id, release_unit)
        {
            selection.diagnostics.push(message);
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

/// Reject shared ownership among packages that resolve unambiguously.
fn validate_package_artifacts(
    root: &Path,
    id: &str,
    release_unit: &ReleaseUnitConfig,
) -> Result<()> {
    let mut owners = BTreeMap::<PathBuf, String>::new();
    for (package_id, package) in &release_unit.packages {
        let mut boundary = release_unit.clone();
        boundary.path = if package.path == Path::new(".") {
            release_unit.path.clone()
        } else {
            release_unit.path.join(&package.path)
        };
        boundary.packages.clear();
        let derived = derive_capabilities(root, &boundary)?;
        let configured = package.publishers().into_iter().collect::<BTreeSet<_>>();
        let candidates = derived
            .iter()
            .filter(|evidence| {
                configured.is_empty()
                    || configured.iter().any(|publisher| {
                        recipes_for(&BTreeSet::from([evidence.capability]), *publisher)
                            .into_iter()
                            .next()
                            .is_some()
                    })
            })
            .collect::<Vec<_>>();
        let artifact = match candidates.as_slice() {
            [evidence] => &evidence.evidence.path,
            _ => continue,
        };
        if let Some(first) = owners.insert(artifact.clone(), package_id.clone()) {
            return Err(Error::Validation(format!(
                "release unit {id} packages {first} and {package_id} resolve to the same native artifact {}",
                artifact.display()
            )));
        }
    }
    Ok(())
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
    if let Some(npm) = release_unit.npm() {
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
    if release_unit.cargo().is_some() {
        targets.push((
            PublisherKind::Cargo,
            PRIMARY_TARGET.to_owned(),
            Configured::default(),
        ));
    }
    if let Some(homebrew) = release_unit.homebrew() {
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
        (PublisherKind::Rpm, release_unit.rpm().is_some()),
        (PublisherKind::Apt, release_unit.apt().is_some()),
        (PublisherKind::Aur, release_unit.aur().is_some()),
    ] {
        if present {
            targets.push((publisher, PRIMARY_TARGET.to_owned(), Configured::default()));
        }
    }
    if let Some(oci) = release_unit.oci() {
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
        // The Arch User Repository resolves a package by name, and GoReleaser's
        // own configuration is where that name lives: explicitly under `aur`,
        // and otherwise as the binary package of the declared project.
        (None, PublisherKind::Aur) => {
            aur_package(root, release_unit, &format!("{id}/{publisher}/{target}"))?
        }
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
        "release unit {} is a Go module but declares no discoverable main package, so it derives no {} capability; the maintained recipes build a command, and the packager finds every directory in the module that declares package main",
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

/// The file inside a release unit one capability probe opens by name.
///
/// Named here rather than beside each probe, because a consumer that must know
/// which paths the derivation reads — evidence assembly proves each of them
/// against the release commit before trusting what it derived — would
/// otherwise restate the names, and a probe added later would leave that
/// restatement silently short. Matched exhaustively, so a capability added
/// later cannot compile without naming the file its probe opens.
pub const fn probe_file(capability: Capability) -> &'static str {
    match capability {
        Capability::NodePackage => "package.json",
        Capability::RustCrate => "Cargo.toml",
        Capability::GoApplication => "go.mod",
        Capability::RunnableImage => "Dockerfile",
        Capability::DevContainerFeature => "devcontainer-feature.json",
    }
}

/// Every path the publication selection opens, relative to the workspace root.
///
/// A consumer that must prove what the selection read needs the paths before
/// the selection runs, and needs them from here rather than from a list of its
/// own. The Go discovery walks for `*.go` files rather than opening one by
/// name, so a caller widens this set with those; every other read is a fixed
/// name under a release unit.
pub fn publication_probe_paths(config: &Config) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for release_unit in config.release_units.values() {
        if !selects_publications(release_unit) {
            continue;
        }
        for capability in Capability::ALL {
            paths.insert(release_unit.path.join(probe_file(capability)));
        }
        for name in Packager::GoReleaser.configuration_paths() {
            paths.insert(release_unit.path.join(name));
        }
    }
    paths
}

/// Derive every publishable capability of one release unit from its native evidence.
pub fn derive_capabilities(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
) -> Result<Vec<CapabilityEvidence>> {
    let mut derived = Vec::new();
    let unit = &release_unit.path;
    if node_package_is_publishable(root, &unit.join(probe_file(Capability::NodePackage)))? {
        derived.push(capability_evidence(
            root,
            Capability::NodePackage,
            unit.join(probe_file(Capability::NodePackage)),
        )?);
    }
    if cargo_manifest(root, &unit.join(probe_file(Capability::RustCrate)))?
        .is_some_and(|manifest| manifest.publishable())
    {
        derived.push(capability_evidence(
            root,
            Capability::RustCrate,
            unit.join(probe_file(Capability::RustCrate)),
        )?);
    }
    if root
        .join(unit)
        .join(probe_file(Capability::GoApplication))
        .is_file()
        && main_package_directory(&root.join(unit))?.is_some()
    {
        derived.push(capability_evidence(
            root,
            Capability::GoApplication,
            unit.join(probe_file(Capability::GoApplication)),
        )?);
    }
    if root
        .join(unit)
        .join(probe_file(Capability::RunnableImage))
        .is_file()
    {
        derived.push(capability_evidence(
            root,
            Capability::RunnableImage,
            unit.join(probe_file(Capability::RunnableImage)),
        )?);
    }
    if root
        .join(unit)
        .join(probe_file(Capability::DevContainerFeature))
        .is_file()
    {
        derived.push(capability_evidence(
            root,
            Capability::DevContainerFeature,
            unit.join(probe_file(Capability::DevContainerFeature)),
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
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(path, error)),
    }
}

fn node_package_is_publishable(root: &Path, relative: &Path) -> Result<bool> {
    let path = root.join(relative);
    if !path.is_file() {
        return Ok(false);
    }
    let Some(text) = probed_file_text(&path)? else {
        return Ok(false);
    };
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
    let Some(text) = probed_file_text(&path)? else {
        return Ok(None);
    };
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

/// Arch User Repository package one release unit publishes, from native evidence.
///
/// The destination is whatever the packager wrote and registered, which is not
/// what the repository declared: the packager resolves an unnamed entry to the
/// project name and then suffixes every name with `-bin` unless it already ends
/// that way. Reading the declaration verbatim would name a package that does not
/// exist, so the same rule the packager applies is applied here.
fn aur_package(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
    identity: &str,
) -> Result<Option<String>> {
    let directory = root.join(&release_unit.path);
    let config = crate::executor::goreleaser::read(&directory)?;
    let Some(project) =
        crate::executor::goreleaser::subject_identity_from(&directory, config.as_ref())?
    else {
        return Ok(None);
    };
    let path = release_unit.path.join(
        config
            .as_ref()
            .map(|config| config.path.as_path())
            .unwrap_or(Path::new(".goreleaser.yaml")),
    );
    let declared_origin = format!("{} aur[0].name", path.display());
    let project_origin = if config
        .as_ref()
        .is_some_and(|config| config.project_name.is_some())
    {
        format!("{} project_name", path.display())
    } else {
        format!(
            "{} module basename",
            release_unit.path.join("go.mod").display()
        )
    };
    let declared = config
        .as_ref()
        .and_then(|config| config.aur_names.first())
        .and_then(Option::as_deref)
        .map(|value| crate::executor::names::SuppliedName {
            origin: &declared_origin,
            value,
        });
    if declared
        .as_ref()
        .is_some_and(|supplied| crate::executor::goreleaser::is_templated_name(supplied.value))
    {
        return Err(Error::Validation(format!(
            "{identity} cannot publish templated aur[0].name in {}; maintained Arch publication requires a literal package name",
            path.display()
        )));
    }
    let project = crate::executor::names::SuppliedName {
        origin: &project_origin,
        value: &project,
    };
    crate::executor::goreleaser::arch_package_name(declared.as_ref(), &project)
        .map(Some)
        .map_err(Error::Validation)
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

/// Where one release unit's Go command package lives, when it can be found.
///
/// Discovery returns the directory rather than a boolean because the reason a
/// capability was withheld has to be reportable: a `go.mod` with no discoverable
/// command is a diagnosable configuration, not an absent Go module.
fn main_package_directory(directory: &Path) -> Result<Option<PathBuf>> {
    Ok(go_main_package_directories(directory)?.into_iter().next())
}

/// Every discoverable main-package directory in one Go module.
pub(crate) fn go_main_package_directories(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for entry in WalkDir::new(directory).into_iter().filter_entry(|entry| {
        entry.depth() == 0 || !entry.file_type().is_dir() || !entry.path().join("go.mod").is_file()
    }) {
        let entry = entry.map_err(|error| {
            Error::Validation(format!(
                "cannot inspect Go module {}: {error}",
                directory.display()
            ))
        })?;
        if entry.file_type().is_dir() && declares_main_package(entry.path())? {
            roots.push(entry.into_path());
        }
    }
    Ok(roots)
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
        let package = publisher
            .lines()
            .map(|line| format!("    {line}\n"))
            .collect::<String>();
        let text = GITHUB.replace(
            "    path: component\n",
            &format!(
                "    path: component\n    packages:\n      package:\n        path: .\n{package}"
            ),
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
    fn permits_coincident_package_paths_for_distinct_native_artifacts() {
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
        let selected = select_publications(workspace.root(), &config)
            .expect("distinct artifacts may share a package path");
        assert_eq!(selected.len(), 2);
    }

    /// The reader names every path the selection opens, for every unit.
    ///
    /// `publication_probe_paths` exists so evidence assembly can prove what the
    /// selection read, and the two agree only if they skip the same release
    /// units. Sharing `selects_publications` makes them agree by construction,
    /// but construction is what a later edit undoes, and neither side's own
    /// tests can see the divergence: the selection would open probe files the
    /// reader never names, the comparison would pass over paths nobody
    /// compared, and every assertion on either side would stay green.
    ///
    /// Held here by outcome rather than by inspection. A workspace carrying
    /// every probe file for a publishing unit, a suspended one and one with no
    /// publisher, with exactly the named paths deleted, must resolve to what an
    /// empty workspace resolves to. A reader that skipped a unit the selection
    /// probes would leave that unit's files in place, and the two results
    /// would differ.
    ///
    /// An outcome test can only see a read whose result reaches the outcome,
    /// and a unit with no configured target derives capabilities and discards
    /// them — so a selection widened onto one reads files nothing compares
    /// while every result stays identical. A suspended unit is not that case:
    /// it configures a target, so widening onto it moves the outcome on its
    /// own. The target-less unit therefore carries a probe file the derivation
    /// cannot parse, because that failure becomes a diagnostic and the read
    /// stops being invisible. Stocking it with well-formed content is what
    /// makes this assertion unable to fail.
    #[test]
    fn names_every_path_the_selection_opens() {
        const UNITS: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  published:
    path: published
    packages:
      package:
        path: .
        npm: {}
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  suspended:
    path: suspended
    disposition: suspended
    packages:
      package:
        path: .
        npm: {}
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  unpublished:
    path: unpublished
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#;
        let config = Config::from_yaml(UNITS).expect("fixture config");
        let stocked = Workspace::new("probe-paths-stocked");
        let bare = Workspace::new("probe-paths-bare");
        for unit in ["published", "suspended", "unpublished"] {
            for capability in Capability::ALL {
                stocked.write(
                    &format!("{unit}/{}", probe_file(capability)),
                    match capability {
                        Capability::NodePackage => {
                            "{\"name\":\"example-component\",\"version\":\"1.0.0\"}"
                        }
                        // A probe file the derivation cannot parse is the only
                        // read whose result survives being discarded. The
                        // target-less unit derives capabilities and throws
                        // them away, so well-formed content there moves no
                        // outcome and a selection widened onto it reads files
                        // nothing compares, invisibly. An unparseable one
                        // becomes a diagnostic, which is how the read makes
                        // itself observable. The suspended unit needs no such
                        // help: it configures a target, so widening onto it
                        // moves the outcome on its own.
                        Capability::RustCrate if unit == "unpublished" => {
                            "this is not valid toml ["
                        }
                        Capability::RustCrate => {
                            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n"
                        }
                        Capability::GoApplication => "module example.test/component\n",
                        Capability::RunnableImage => "FROM scratch\n",
                        Capability::DevContainerFeature => "{\"id\":\"example\"}\n",
                    },
                );
            }
            for name in Packager::GoReleaser.configuration_paths() {
                stocked.write(&format!("{unit}/{name}"), "builds:\n  - main: ./cmd/tool\n");
            }
        }
        for path in publication_probe_paths(&config) {
            let path = stocked.root().join(path);
            if path.is_file() {
                std::fs::remove_file(&path).expect("remove a named path");
            }
        }

        let stripped = resolve_publications(stocked.root(), &config).expect("resolves");
        let empty = resolve_publications(bare.root(), &config).expect("resolves");
        assert_eq!(
            (identities(&stripped.selected), stripped.diagnostics),
            (identities(&empty.selected), empty.diagnostics),
            "the selection opens a probe file the reader does not name"
        );

        // The other direction costs nothing to hold and is not harmless: a
        // path named but never opened is a path evidence assembly refuses a
        // release over without ever having read it.
        let probes = publication_probe_paths(&config);
        let named: BTreeSet<&Path> = probes.iter().filter_map(|path| path.parent()).collect();
        let opened: BTreeSet<&Path> = config
            .release_units
            .values()
            .filter(|release_unit| selects_publications(release_unit))
            .map(|release_unit| release_unit.path.as_path())
            .collect();
        assert_eq!(
            named, opened,
            "the reader names a release unit the selection never opens"
        );
    }

    /// Publication identities, in the order the selection produced them.
    fn identities(selected: &[SelectedPublication]) -> Vec<String> {
        selected.iter().map(SelectedPublication::identity).collect()
    }

    /// Every capability the derivation can produce is one this module lists.
    ///
    /// Evidence assembly proves each path this selection opens against the
    /// release commit, and it asks for those paths here rather than restating
    /// them. `probe_file` matches exhaustively, so a capability added later
    /// cannot compile without naming its file — but `ALL` could still be left
    /// short, and a capability missing from it is a read assembly would never
    /// prove. A workspace carrying every probe file derives one capability per
    /// entry, which is what binds the list to the derivation rather than to a
    /// count written beside it.
    #[test]
    fn derives_one_capability_for_every_probe_it_enumerates() {
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
        let derived = derive_capabilities(workspace.root(), &config("").release_units["component"])
            .expect("capabilities derive");
        assert_eq!(
            capability_set(&derived),
            BTreeSet::from(Capability::ALL),
            "the derivation produces exactly the capabilities the probe list enumerates"
        );
        let probes: BTreeSet<&str> = Capability::ALL.into_iter().map(probe_file).collect();
        assert_eq!(
            probes.len(),
            Capability::ALL.len(),
            "each capability probes a file of its own"
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
    const GO_LAYOUTS: [(&str, &[(&str, &str)]); 8] = [
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
            "a deeply nested command",
            &[(
                "component/cmd/a/b/c/d/main.go",
                "package main\n\nfunc main() {}\n",
            )],
        ),
        (
            "a command outside conventional roots",
            &[(
                "component/apps/example/main.go",
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
            "an ellipsis import path naming every command beneath a prefix",
            &[
                (
                    "component/.goreleaser.yaml",
                    "version: 2\nbuilds:\n  - main: ./tools/...\n",
                ),
                (
                    "component/tools/nested/example/main.go",
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

    // The destination the recipe is handed has to be the package the packager
    // wrote and the Arch User Repository carries, not the one the repository
    // declared. Reading the declaration verbatim names a package that does not
    // exist, and would push one project's sources to another project's package.
    #[test]
    fn derives_the_arch_package_the_packager_registers() {
        for (declaration, expected) in [
            ("aur:\n  - name: example-tool\n", "example-tool-bin"),
            ("aur:\n  - name: example-tool-bin\n", "example-tool-bin"),
            // An unnamed entry takes the project name, and it must keep its
            // position: the sibling below names itself and must not be read as
            // this publication's destination.
            (
                "aur:\n  - {}\n  - name: example-other\n",
                "example-tool-bin",
            ),
            ("", "example-tool-bin"),
        ] {
            let workspace = Workspace::new("aur-destination");
            workspace
                .write("component/go.mod", "module example.test/example-tool\n")
                .write("component/main.go", "package main\n\nfunc main() {}\n")
                .write(
                    "component/.goreleaser.yaml",
                    &format!("version: 2\nproject_name: example-tool\n{declaration}"),
                );
            let selected = select_publications(workspace.root(), &config("    aur: {}\n"))
                .expect("the publication selects");
            assert_eq!(
                selected[0].destination.as_deref(),
                Some(expected),
                "{declaration:?} derives the package the packager registers"
            );
        }
    }

    #[test]
    fn refuses_a_malformed_declared_arch_package_at_the_boundary() {
        let workspace = Workspace::new("aur-declared-name-boundary");
        workspace
            .write("component/go.mod", "module example.test/example-tool\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n")
            .write(
                "component/.goreleaser.yaml",
                "version: 2\nproject_name: example-tool\naur:\n  - name: invalid/name\n",
            );
        let error = select_publications(workspace.root(), &config("    aur: {}\n"))
            .expect_err("a malformed declared Arch package is refused");
        assert!(
            error
                .to_string()
                .contains("component/.goreleaser.yaml aur[0].name is not an Arch package name"),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_templated_arch_package_before_it_becomes_a_destination() {
        let workspace = Workspace::new("aur-template-boundary");
        workspace
            .write("component/go.mod", "module example.test/example-tool\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n")
            .write(
                "component/.goreleaser.yaml",
                "version: 2\nproject_name: example-tool\naur:\n  - name: '{{ .ProjectName }}/../../other'\n",
            );
        let error = select_publications(workspace.root(), &config("    aur: {}\n"))
            .expect_err("a templated Arch package never becomes a destination");
        assert!(
            error.to_string().contains(
                "component/aur/primary cannot publish templated aur[0].name in component/.goreleaser.yaml"
            ),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_malformed_fallback_arch_package_at_the_boundary() {
        let workspace = Workspace::new("aur-project-name-boundary");
        workspace
            .write("component/go.mod", "module example.test/example-tool\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n")
            .write(
                "component/.goreleaser.yaml",
                "version: 2\nproject_name: invalid/name\naur:\n  - {}\n",
            );
        let error = select_publications(workspace.root(), &config("    aur: {}\n"))
            .expect_err("a malformed project-name fallback is refused");
        assert!(
            error
                .to_string()
                .contains("component/.goreleaser.yaml project_name is not an Arch package name"),
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

    /// Attempts the racing harness makes before it concludes nothing raced.
    ///
    /// The bound caps the test's runtime. It is not a sample size the result
    /// depends on: the probe does not retry, so a removal it sees is resolved on
    /// the spot and one observation settles the question. What the bound has to
    /// be large enough for is the harness's own liveness check, which needs the
    /// competitor to be caught publishing at least once.
    const RACING_ATTEMPTS: usize = 20_000;

    /// Run one probe against a file another thread keeps taking away.
    ///
    /// The competitor publishes and withdraws the file continuously, so the
    /// window between the probe's existence check and its read is reopened for
    /// every attempt. Any error the probe returns is the defect: the file was
    /// there, then it was not, which is the interleaving a concurrently dropped
    /// fixture workspace produces.
    ///
    /// Absence is also what a competitor that never publishes produces, and it
    /// is what every filesystem call here would produce if it started failing,
    /// because each one is discarded. `present` distinguishes the two: the run
    /// has to catch the file published at least once, or the race it reports
    /// surviving was never run.
    fn under_removal<T>(
        label: &str,
        file: &str,
        probe: impl Fn(&Path, &Path) -> Result<T>,
        present: impl Fn(&T) -> bool,
        contents: &str,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let workspace = Workspace::new(label);
        let root = workspace.root().to_path_buf();
        let path = root.join(file);
        let stop = Arc::new(AtomicBool::new(false));
        let staged = root.join("staged-contents");
        std::fs::write(&staged, contents).expect("stage the contents the competitor publishes");
        let competitor = {
            let stop = Arc::clone(&stop);
            let staged = staged.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // Published by rename and withdrawn by removal, so the
                    // probe sees the file whole or not at all. This harness
                    // opens the removal window, not a torn read.
                    let scratch = path.with_extension("staging");
                    let _ = std::fs::copy(&staged, &scratch);
                    let _ = std::fs::rename(&scratch, &path);
                    let _ = std::fs::remove_file(&path);
                }
            })
        };

        let relative = Path::new(file);
        let mut observed_present = 0_usize;
        let mut failure = None;
        for _ in 0..RACING_ATTEMPTS {
            match probe(&root, relative) {
                Ok(answer) if present(&answer) => observed_present += 1,
                Ok(_) => {}
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }

        stop.store(true, Ordering::Relaxed);
        competitor.join().expect("the competing thread finishes");

        if let Some(error) = failure {
            panic!("{file} was removed while the probe ran and became a failure: {error:?}");
        }
        assert!(
            observed_present > 0,
            "the competitor never published {file}, so no removal was ever raced and this run proves nothing"
        );
    }

    /// The window the CLI handoff fixtures fell into, opened on purpose.
    #[test]
    fn a_cargo_manifest_removed_while_the_probe_runs_is_absent_rather_than_an_error() {
        under_removal(
            "cargo-manifest-removal-race",
            "Cargo.toml",
            cargo_manifest,
            std::option::Option::is_some,
            "[package]\nname = \"raced-component\"\nversion = \"1.0.0\"\n",
        );
    }

    /// The same window, in the probe that shares the check-then-read shape.
    #[test]
    fn a_package_manifest_removed_while_the_probe_runs_is_absent_rather_than_an_error() {
        under_removal(
            "package-manifest-removal-race",
            "package.json",
            node_package_is_publishable,
            |publishable| *publishable,
            r#"{"name":"raced-component","version":"1.0.0"}"#,
        );
    }
}
