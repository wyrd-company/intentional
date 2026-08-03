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
        retrieval: recipe.retrieval,
    })
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
    if root.join(unit).join("go.mod").is_file() && has_main_package(&root.join(unit))? {
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

fn has_main_package(directory: &Path) -> Result<bool> {
    let mut roots = vec![directory.to_owned()];
    let commands = directory.join("cmd");
    if commands.is_dir() {
        for entry in std::fs::read_dir(&commands).map_err(|error| Error::io(&commands, error))? {
            let entry = entry.map_err(|error| Error::io(&commands, error))?;
            if entry.path().is_dir() {
                roots.push(entry.path());
            }
        }
    }
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(|error| Error::io(&root, error))?;
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "go") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text
                .lines()
                .any(|line| line.trim_end() == "package main" || line.trim_end() == "package main;")
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
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
