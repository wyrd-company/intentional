// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace configuration and validation.

use crate::error::{Error, Result};
use crate::model::{
    Adapter, AttachedComponent, Bump, Pre1BumpMapping, ProjectionMode, PublisherKind,
    ReleaseUnitDisposition, TagPhase, TagRole,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Location of the workspace configuration.
pub const CONFIG_PATH: &str = ".intentional/config.yml";

/// Published configuration schema identifier.
pub const CONFIG_SCHEMA: &str = "https://intentional.foo/schemas/config.yml";

/// Current interpretation contract written by initialization.
pub const CURRENT_CONTRACT: &str = "contract-2";

/// Whether this binary can interpret a historical release document contract.
pub fn supports_interpretation_contract(contract: &str) -> bool {
    matches!(contract, "contract-1" | "contract-2")
}

/// Complete workspace configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// Optional schema URL for editor and validation tooling.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Versioned release-semantics contract.
    pub contract: String,
    /// Workspace-wide release settings.
    #[serde(default)]
    pub settings: Settings,
    /// Fixed release groups using Changesets semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<Vec<String>>,
    /// Linked release groups using Changesets semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked: Vec<Vec<String>>,
    /// Repository-level release tag streams.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_tags: BTreeMap<String, WorkspaceTagConfig>,
    /// Receipts for discovery candidates already resolved by initialization.
    #[serde(default, skip_serializing_if = "DiscoveryConfig::is_empty")]
    pub discovery: DiscoveryConfig,
    /// Opt-in GitHub executor integration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GithubConfig>,
    /// Release-unit inventory keyed by stable release-unit id.
    pub release_units: BTreeMap<String, ReleaseUnitConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: Some(CONFIG_SCHEMA.to_owned()),
            contract: CURRENT_CONTRACT.to_owned(),
            settings: Settings::default(),
            fixed: Vec::new(),
            linked: Vec::new(),
            workspace_tags: BTreeMap::new(),
            discovery: DiscoveryConfig::default(),
            github: None,
            release_units: BTreeMap::new(),
        }
    }
}

/// Durable receipts for resolved discovery candidates.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DiscoveryConfig {
    /// Candidate paths incorporated into configured release units.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_paths: Vec<ManagedPathReceipt>,
    /// Candidate paths explicitly excluded at an exact evidence digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_paths: Vec<ExcludedPathReceipt>,
}

impl DiscoveryConfig {
    fn is_empty(&self) -> bool {
        self.managed_paths.is_empty() && self.excluded_paths.is_empty()
    }
}

/// Receipt connecting one detector/path identity to a configured release unit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ManagedPathReceipt {
    /// Stable detector id.
    pub detector: String,
    /// Exact workspace-relative candidate path.
    pub path: PathBuf,
    /// Configured release unit that owns the candidate projection.
    pub release_unit: String,
}

/// Receipt excluding one detector/path identity while its evidence is unchanged.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExcludedPathReceipt {
    /// Stable detector id.
    pub detector: String,
    /// Exact workspace-relative candidate path.
    pub path: PathBuf,
    /// SHA-256 digest of the evidence at the time of exclusion.
    pub evidence_digest: String,
}

/// Default repository-owned release workflow proposed by executor initialization.
pub const DEFAULT_RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";

/// Default repository-owned publish workflow proposed by executor initialization.
pub const DEFAULT_PUBLISH_WORKFLOW: &str = ".github/workflows/publish.yml";

/// Job and step identifier namespace reserved when no prefix is configured.
pub const DEFAULT_JOB_PREFIX: &str = "intentional_";

/// Environment variable namespace reserved when no prefix is configured.
pub const DEFAULT_ENVVAR_PREFIX: &str = "INTENTIONAL_";

/// GitHub executor opt-in and its repository-owned workflow identities.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GithubConfig {
    /// Repository-owned workflows carrying Intentional-managed slices.
    pub workflows: GithubWorkflows,
    /// Reserved job, step, and environment variable namespaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<ExecutorPrefix>,
}

impl GithubConfig {
    /// Resolve the reserved identifier namespaces for this configuration.
    pub fn namespaces(&self) -> Result<PrefixNamespaces> {
        match &self.prefix {
            Some(prefix) => prefix.namespaces(),
            None => PrefixNamespaces::new(DEFAULT_JOB_PREFIX, DEFAULT_ENVVAR_PREFIX),
        }
    }

    /// Workflow configuration for one executor role.
    pub fn workflow(&self, role: WorkflowRole) -> &GithubWorkflow {
        match role {
            WorkflowRole::Release => &self.workflows.release,
            WorkflowRole::Publish => &self.workflows.publish,
        }
    }
}

/// Executor role of a repository-owned workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkflowRole {
    /// Workflow that seals a release plan and performs the release authority transition.
    Release,
    /// Tag-triggered workflow that publishes and closes the release.
    Publish,
}

impl WorkflowRole {
    /// Stable lowercase name used by commands, diagnostics, and plans.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Publish => "publish",
        }
    }

    /// Both executor roles in stable order.
    pub const ALL: [Self; 2] = [Self::Release, Self::Publish];
}

impl std::fmt::Display for WorkflowRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkflowRole {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "release" => Ok(Self::Release),
            "publish" => Ok(Self::Publish),
            _ => Err(format!("expected release or publish; got {value}")),
        }
    }
}

/// Repository-owned workflows managed by the GitHub executor.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GithubWorkflows {
    /// Workflow performing release preparation and the authority transition.
    pub release: GithubWorkflow,
    /// Workflow performing publication and immutable Release closure.
    pub publish: GithubWorkflow,
}

/// One repository-owned workflow and the gates gating its authority transition.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GithubWorkflow {
    /// Exact workspace-relative workflow path used as the command default.
    pub path: PathBuf,
    /// Repository-owned job identifiers that gate the managed transition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<String>,
}

/// Configured reservation of the managed identifier namespaces.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExecutorPrefix {
    /// One name normalized into both namespaces.
    Scalar(String),
    /// Independently configured job and environment variable namespaces.
    Namespaces {
        /// Job and step identifier namespace.
        job: String,
        /// Environment variable namespace.
        envvar: String,
    },
}

impl ExecutorPrefix {
    /// Normalize the configured prefix into validated namespaces.
    pub fn namespaces(&self) -> Result<PrefixNamespaces> {
        match self {
            Self::Scalar(value) => {
                let words = identifier_words(value);
                if words.is_empty() {
                    return Err(Error::Validation(format!(
                        "github prefix {value:?} contains no identifier characters"
                    )));
                }
                PrefixNamespaces::new(
                    &format!("{}_", words.join("_").to_lowercase()),
                    &format!("{}_", words.join("_").to_uppercase()),
                )
            }
            Self::Namespaces { job, envvar } => {
                PrefixNamespaces::new(&separated(job), &separated(envvar))
            }
        }
    }
}

/// Validated identifier namespaces reserved by the executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixNamespaces {
    /// Prefix reserved for managed job and step identifiers.
    pub job: String,
    /// Prefix reserved for managed environment variables.
    pub envvar: String,
    /// Protected GitHub environment guarding the release authority transition.
    pub environment: String,
}

impl PrefixNamespaces {
    fn new(job: &str, envvar: &str) -> Result<Self> {
        validate_namespace(job, "github job prefix", true)?;
        validate_namespace(envvar, "github envvar prefix", false)?;
        let environment =
            format!("{}-release", job.trim_end_matches('_').replace('_', "-")).to_lowercase();
        if !environment
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(Error::Validation(format!(
                "github job prefix {job:?} does not derive a usable release environment name"
            )));
        }
        Ok(Self {
            job: job.to_owned(),
            envvar: envvar.to_owned(),
            environment,
        })
    }
}

fn separated(value: &str) -> String {
    if value.ends_with('_') {
        value.to_owned()
    } else {
        format!("{value}_")
    }
}

fn identifier_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower_or_digit = false;
    for character in value.chars() {
        if !character.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower_or_digit = false;
            continue;
        }
        if character.is_ascii_uppercase() && previous_lower_or_digit && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        previous_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn validate_namespace(value: &str, description: &str, allow_hyphen: bool) -> Result<()> {
    let body = value.trim_end_matches('_');
    let valid_start = body
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_');
    let valid_body = body.chars().all(|character| {
        character.is_ascii_alphanumeric() || character == '_' || (allow_hyphen && character == '-')
    });
    if body.is_empty() || !valid_start || !valid_body {
        return Err(Error::Validation(format!(
            "{description} {value:?} must normalize to an identifier starting with a letter"
        )));
    }
    Ok(())
}

/// Workspace-wide release behavior.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Settings {
    /// Minimum bump propagated to internal dependents.
    #[serde(default = "default_dependency_bump")]
    pub internal_dependency_bump: Bump,
    /// Interpretation of bump names before semantic version 1.0.0.
    #[serde(default)]
    pub pre_1_0_bump_mapping: Pre1BumpMapping,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            internal_dependency_bump: default_dependency_bump(),
            pre_1_0_bump_mapping: Pre1BumpMapping::default(),
        }
    }
}

const fn default_dependency_bump() -> Bump {
    Bump::Patch
}

/// One independently versioned release unit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleaseUnitConfig {
    /// Release-unit directory relative to the workspace root.
    pub path: PathBuf,
    /// Whether releases may include this release unit.
    #[serde(default, skip_serializing_if = "is_managed")]
    pub disposition: ReleaseUnitDisposition,
    /// Version-bearing ecosystem and format projections. Empty means tag-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projections: Vec<Projection>,
    /// Named release-unit tag streams. Exactly one is primary.
    pub tags: BTreeMap<String, TagConfig>,
    /// Authored internal dependency edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Named packaging boundaries inside this versioning boundary.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub packages: BTreeMap<String, PackageConfig>,
}

/// One package that publishes its release unit's version.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PackageConfig {
    /// Package directory relative to its release unit.
    pub path: PathBuf,
    /// Explicit npm publication intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm: Option<NpmPublisher>,
    /// Explicit Cargo publication intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo: Option<CargoPublisher>,
    /// Explicit Homebrew publication intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homebrew: Option<HomebrewPublisher>,
    /// Explicit RPM publication intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<SystemPackagePublisher>,
    /// Explicit APT publication intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apt: Option<SystemPackagePublisher>,
    /// Explicit Arch User Repository publication intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aur: Option<SystemPackagePublisher>,
    /// Explicit OCI publication intent with at least one peer target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci: Option<OciPublisher>,
}

impl PackageConfig {
    /// Empty package declaration at a release-unit-relative path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            npm: None,
            cargo: None,
            homebrew: None,
            rpm: None,
            apt: None,
            aur: None,
            oci: None,
        }
    }

    /// Publishers this package explicitly opts into, in stable order.
    pub fn publishers(&self) -> Vec<PublisherKind> {
        [
            (PublisherKind::Npm, self.npm.is_some()),
            (PublisherKind::Cargo, self.cargo.is_some()),
            (PublisherKind::Homebrew, self.homebrew.is_some()),
            (PublisherKind::Rpm, self.rpm.is_some()),
            (PublisherKind::Apt, self.apt.is_some()),
            (PublisherKind::Aur, self.aur.is_some()),
            (PublisherKind::Oci, self.oci.is_some()),
        ]
        .into_iter()
        .filter_map(|(publisher, configured)| configured.then_some(publisher))
        .collect()
    }
}

impl ReleaseUnitConfig {
    fn package_with<T>(&self, select: impl Fn(&PackageConfig) -> Option<&T>) -> Option<&T> {
        self.packages.values().find_map(select)
    }

    /// First configured npm publisher in stable package order.
    pub fn npm(&self) -> Option<&NpmPublisher> {
        self.package_with(|package| package.npm.as_ref())
    }

    /// First configured Cargo publisher in stable package order.
    pub fn cargo(&self) -> Option<&CargoPublisher> {
        self.package_with(|package| package.cargo.as_ref())
    }

    /// First configured Homebrew publisher in stable package order.
    pub fn homebrew(&self) -> Option<&HomebrewPublisher> {
        self.package_with(|package| package.homebrew.as_ref())
    }

    /// First configured RPM publisher in stable package order.
    pub fn rpm(&self) -> Option<&SystemPackagePublisher> {
        self.package_with(|package| package.rpm.as_ref())
    }

    /// First configured APT publisher in stable package order.
    pub fn apt(&self) -> Option<&SystemPackagePublisher> {
        self.package_with(|package| package.apt.as_ref())
    }

    /// First configured AUR publisher in stable package order.
    pub fn aur(&self) -> Option<&SystemPackagePublisher> {
        self.package_with(|package| package.aur.as_ref())
    }

    /// First configured OCI publisher in stable package order.
    pub fn oci(&self) -> Option<&OciPublisher> {
        self.package_with(|package| package.oci.as_ref())
    }

    /// Managed release unit without dependency edges or publication intent.
    pub fn managed(
        path: PathBuf,
        projections: Vec<Projection>,
        tags: BTreeMap<String, TagConfig>,
    ) -> Self {
        Self {
            path,
            disposition: ReleaseUnitDisposition::Managed,
            projections,
            tags,
            depends_on: Vec::new(),
            packages: BTreeMap::new(),
        }
    }

    /// Publishers this release unit explicitly opts into, in stable order.
    pub fn publishers(&self) -> Vec<PublisherKind> {
        self.packages
            .values()
            .flat_map(PackageConfig::publishers)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// npm publication intent; an empty mapping selects the npmjs primary destination.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NpmPublisher {
    /// GitHub secret name holding the bootstrap npm token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_secret: Option<String>,
    /// Destinations published in addition to the npmjs primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_targets: Option<NpmAdditionalTargets>,
}

/// npm destinations published alongside the primary.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NpmAdditionalTargets {
    /// GitHub Package Registry destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<NpmGithubTarget>,
}

/// GitHub Package Registry destination; identity derives from GitHub and package evidence.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct NpmGithubTarget {}

/// Cargo publication intent; an empty mapping selects the native primary registry.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CargoPublisher {
    /// GitHub secret name holding the bootstrap registry token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_secret: Option<String>,
}

/// Homebrew publication intent; formula identity derives from release-unit evidence.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct HomebrewPublisher {
    /// Tap repository receiving the generated formula.
    pub repository: String,
}

/// RPM, APT, and AUR publication intent carried entirely by native packager configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SystemPackagePublisher {}

/// OCI publication intent; every destination is an explicit peer target.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OciPublisher {
    /// Docker Hub destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerhub: Option<DockerhubTarget>,
    /// GitHub Container Registry destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ghcr: Option<GhcrTarget>,
}

/// Docker Hub OCI destination.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DockerhubTarget {
    /// Docker Hub repository when the identity is not derivable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// GitHub variable name holding the Docker Hub username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username_var: Option<String>,
    /// GitHub secret name holding the Docker Hub access token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_secret: Option<String>,
    /// Attached components this destination suppresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omit: Vec<AttachedComponent>,
}

/// GitHub Container Registry OCI destination.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GhcrTarget {
    /// Registry repository when the derived owner and subject are overridden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Attached components this destination suppresses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omit: Vec<AttachedComponent>,
}

fn is_managed(disposition: &ReleaseUnitDisposition) -> bool {
    *disposition == ReleaseUnitDisposition::Managed
}

/// One named release-unit tag.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TagConfig {
    /// Whether this tag supplies version authority or projects it.
    pub role: TagRole,
    /// Tag name template containing `{version}` and optionally `{id}`.
    pub template: String,
    /// Optional executor phase declaration required for creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_phase: Option<TagPhase>,
    /// Observable tag prerequisites expressed as canonical tag ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag_after: Vec<String>,
}

/// One named workspace-level tag.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WorkspaceTagConfig {
    /// Tag name template containing `{version}`.
    pub template: String,
    /// Optional executor phase declaration required for creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_phase: Option<TagPhase>,
    /// Observable tag prerequisites expressed as canonical tag ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag_after: Vec<String>,
}

/// One workspace tag that declares no executor phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnphasedTag {
    /// Canonical tag id.
    pub id: String,
    /// Tag name template with the release-unit id already resolved.
    pub template: String,
}

/// A version projection into a manifest or arbitrary file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Projection {
    /// Adapter specialization.
    pub adapter: Adapter,
    /// File relative to the release-unit path.
    pub file: PathBuf,
    /// Projection materialization mode.
    pub mode: ProjectionMode,
    /// JSON Pointer or dotted TOML/YAML key path for generic formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
}

impl Config {
    /// Load and validate configuration rooted at `root`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_PATH);
        let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
        Self::from_yaml(&text)
    }

    /// Parse and validate configuration YAML.
    pub fn from_yaml(text: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct ContractHeader {
            contract: String,
        }

        let header: ContractHeader = serde_yaml::from_str(text)?;
        if header.contract == "contract-1" {
            return Err(Error::Validation(
                "unsupported interpretation contract \"contract-1\"; migrate to contract-2 by adding release-units.<unit>.packages and moving each publisher mapping beneath the package it publishes"
                    .to_owned(),
            ));
        }
        if header.contract != CURRENT_CONTRACT {
            return Err(Error::Validation(format!(
                "unsupported interpretation contract {:?}; expected {CURRENT_CONTRACT}",
                header.contract
            )));
        }
        let config: Self = serde_yaml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Serialize configuration deterministically.
    pub fn to_yaml(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_yaml::to_string(self)?)
    }

    /// Return the configured primary tag for a release unit.
    pub fn primary_tag<'a>(&'a self, release_unit_id: &str) -> Result<(&'a str, &'a TagConfig)> {
        let release_unit = self
            .release_units
            .get(release_unit_id)
            .ok_or_else(|| Error::Validation(format!("unknown release unit {release_unit_id}")))?;
        release_unit
            .tags
            .iter()
            .find(|(_, tag)| tag.role == TagRole::Primary)
            .map(|(id, tag)| (id.as_str(), tag))
            .ok_or_else(|| {
                Error::Validation(format!("release unit {release_unit_id} has no primary tag"))
            })
    }

    /// Workspace tags that declare no executor phase, in stable order.
    ///
    /// The release workflow publishes exactly one annotated global release tag
    /// with the release commit, and every other configured tag declares the
    /// phase that creates it. The unphased tag is therefore the global release
    /// tag: executor conformance requires exactly one, and the publish workflow
    /// is triggered by the name it renders.
    ///
    /// Only workspace tags are candidates. A release plan seals the workspace
    /// tags together with the tags of the release units that release, so a
    /// release-unit tag is present in some plans and absent from others. A
    /// global release tag that appears or disappears with the composition of a
    /// release cannot trigger publication, and the defect surfaces at
    /// preparation rather than at configuration. Executor conformance reports an
    /// unphased release-unit tag separately; see
    /// [`Config::unphased_release_unit_tags`].
    pub fn unphased_tags(&self) -> Vec<UnphasedTag> {
        let mut unphased = Vec::new();
        for (tag_id, tag) in &self.workspace_tags {
            if tag.require_phase.is_none() {
                unphased.push(UnphasedTag {
                    id: Self::workspace_tag_id(tag_id),
                    template: tag.template.clone(),
                });
            }
        }
        unphased
    }

    /// Canonical ids of release-unit tags that declare no executor phase.
    ///
    /// These cannot be the global release tag, because no release unit is in
    /// every release. Reporting them is what stops a configuration from passing
    /// conformance one release at a time and failing preparation on the next.
    pub fn unphased_release_unit_tags(&self) -> Vec<String> {
        let mut unphased = Vec::new();
        for (release_unit_id, release_unit) in &self.release_units {
            for (tag_id, tag) in &release_unit.tags {
                if tag.require_phase.is_none() {
                    unphased.push(Self::release_unit_tag_id(release_unit_id, tag_id));
                }
            }
        }
        unphased
    }

    /// Executor phases the configured tags declare, in phase order.
    ///
    /// A phase with no configured tag seals nothing, so the derived publication
    /// graph carries a tag job only for a phase this configuration actually
    /// declares. Deriving one regardless would produce a job whose command
    /// refuses to run.
    pub fn declared_phases(&self) -> Vec<TagPhase> {
        let mut declared = Vec::new();
        for release_unit in self.release_units.values() {
            for tag in release_unit.tags.values() {
                declared.extend(tag.require_phase);
            }
        }
        for tag in self.workspace_tags.values() {
            declared.extend(tag.require_phase);
        }
        [TagPhase::BeforePublication, TagPhase::AfterPublication]
            .into_iter()
            .filter(|phase| declared.contains(phase))
            .collect()
    }

    /// Canonical id for a named release-unit tag.
    pub fn release_unit_tag_id(release_unit_id: &str, tag_id: &str) -> String {
        format!("release-unit/{release_unit_id}/{tag_id}")
    }

    /// Canonical id for a named workspace tag.
    pub fn workspace_tag_id(tag_id: &str) -> String {
        format!("workspace/{tag_id}")
    }

    /// Validate release-unit ids, projections, release groups, tags, and dependency graphs.
    pub fn validate(&self) -> Result<()> {
        if self.contract != CURRENT_CONTRACT {
            return Err(Error::Validation(format!(
                "unsupported interpretation contract {:?}; expected {CURRENT_CONTRACT}",
                self.contract
            )));
        }
        if self.release_units.is_empty() {
            return Err(Error::Validation(
                "config must declare at least one release unit".to_owned(),
            ));
        }
        if self.settings.internal_dependency_bump == Bump::None {
            return Err(Error::Validation(
                "internal-dependency-bump must be major, minor, or patch".to_owned(),
            ));
        }

        let mut canonical_tags = BTreeSet::new();
        let mut resolved_templates = BTreeMap::new();
        for (id, release_unit) in &self.release_units {
            validate_id(id, "release unit")?;
            validate_relative_path(&release_unit.path, &format!("release unit {id} path"))?;
            for (package_id, package) in &release_unit.packages {
                validate_id(package_id, "package")?;
                validate_relative_path(
                    &package.path,
                    &format!("release unit {id} package {package_id} path"),
                )?;
            }
            let primary_count = release_unit
                .tags
                .values()
                .filter(|tag| tag.role == TagRole::Primary)
                .count();
            if primary_count != 1 {
                return Err(Error::Validation(format!(
                    "release unit {id} must declare exactly one primary tag"
                )));
            }

            let mut projection_keys = BTreeSet::new();
            for projection in &release_unit.projections {
                validate_relative_path(
                    &projection.file,
                    &format!("release unit {id} projection file"),
                )?;
                if projection.adapter.requires_pointer()
                    && projection.pointer.as_deref().is_none_or(str::is_empty)
                {
                    return Err(Error::Validation(format!(
                        "release unit {id} generic {:?} projection requires a pointer",
                        projection.adapter
                    )));
                }
                let key = (&projection.file, projection.pointer.as_deref());
                if !projection_keys.insert(key) {
                    return Err(Error::Validation(format!(
                        "release unit {id} repeats projection {}",
                        projection.file.display()
                    )));
                }
            }

            validate_dependencies(id, release_unit, &self.release_units)?;
            for (tag_id, tag) in &release_unit.tags {
                validate_id(tag_id, "tag")?;
                validate_tag_template(
                    &format!("release unit {id} tag {tag_id}"),
                    &tag.template,
                    true,
                )?;
                let canonical = Self::release_unit_tag_id(id, tag_id);
                canonical_tags.insert(canonical);
                let rendered = tag.template.replace("{id}", id);
                if let Some(other) =
                    resolved_templates.insert(rendered, format!("release unit {id} tag {tag_id}"))
                {
                    return Err(Error::Validation(format!(
                        "release unit {id} tag {tag_id} template collides with {other}"
                    )));
                }
            }
        }

        for (id, tag) in &self.workspace_tags {
            validate_id(id, "workspace tag")?;
            validate_tag_template(&format!("workspace tag {id}"), &tag.template, false)?;
            canonical_tags.insert(Self::workspace_tag_id(id));
            if let Some(other) =
                resolved_templates.insert(tag.template.clone(), format!("workspace tag {id}"))
            {
                return Err(Error::Validation(format!(
                    "workspace tag {id} template collides with {other}"
                )));
            }
        }

        self.validate_discovery()?;
        self.validate_github()?;
        self.validate_publishers()?;
        self.validate_release_groups()?;
        self.validate_dependency_acyclic()?;
        self.validate_tag_graph(&canonical_tags)
    }

    fn validate_discovery(&self) -> Result<()> {
        let mut identities = BTreeSet::new();
        for receipt in &self.discovery.managed_paths {
            validate_detector_id(&receipt.detector)?;
            validate_exact_discovery_path(&receipt.path, "managed discovery path")?;
            validate_id(&receipt.release_unit, "managed discovery release unit")?;
            if !self.release_units.contains_key(&receipt.release_unit) {
                return Err(Error::Validation(format!(
                    "managed discovery path {} references unknown release unit {}",
                    receipt.path.display(),
                    receipt.release_unit
                )));
            }
            if !identities.insert((&receipt.detector, &receipt.path)) {
                return Err(Error::Validation(format!(
                    "duplicate discovery receipt for detector {} at {}",
                    receipt.detector,
                    receipt.path.display()
                )));
            }
        }
        for receipt in &self.discovery.excluded_paths {
            validate_detector_id(&receipt.detector)?;
            validate_exact_discovery_path(&receipt.path, "excluded discovery path")?;
            validate_sha256(
                &receipt.evidence_digest,
                "excluded discovery evidence digest",
            )?;
            if !identities.insert((&receipt.detector, &receipt.path)) {
                return Err(Error::Validation(format!(
                    "duplicate discovery receipt for detector {} at {}",
                    receipt.detector,
                    receipt.path.display()
                )));
            }
        }
        Ok(())
    }

    fn validate_github(&self) -> Result<()> {
        let Some(github) = &self.github else {
            return Ok(());
        };
        let namespaces = github.namespaces()?;
        for role in WorkflowRole::ALL {
            let workflow = github.workflow(role);
            validate_exact_discovery_path(&workflow.path, &format!("github {role} workflow path"))?;
            let mut gates = BTreeSet::new();
            for gate in &workflow.gates {
                validate_workflow_job_id(gate, &format!("github {role} workflow gate"))?;
                if gate.starts_with(&namespaces.job) {
                    return Err(Error::Validation(format!(
                        "github {role} workflow gate {gate} uses the reserved job prefix {}",
                        namespaces.job
                    )));
                }
                if !gates.insert(gate) {
                    return Err(Error::Validation(format!(
                        "github {role} workflow repeats gate {gate}"
                    )));
                }
            }
        }
        if github.workflows.release.path == github.workflows.publish.path {
            return Err(Error::Validation(format!(
                "github release and publish workflows must be distinct files; both name {}",
                github.workflows.release.path.display()
            )));
        }
        Ok(())
    }

    fn validate_publishers(&self) -> Result<()> {
        for (id, release_unit) in &self.release_units {
            for (package_id, package) in &release_unit.packages {
                let publishers = package.publishers();
                if self.github.is_none() {
                    if let Some(publisher) = publishers.first() {
                        return Err(Error::Validation(format!(
                            "release unit {id} package {package_id} configures the {publisher} publisher; a configured publisher requires GitHub executor configuration"
                        )));
                    }
                    continue;
                }
                validate_package_publishers(id, package_id, package)?;
            }
        }
        Ok(())
    }

    fn validate_release_groups(&self) -> Result<()> {
        let mut assigned = BTreeMap::new();
        for (kind, groups) in [("fixed", &self.fixed), ("linked", &self.linked)] {
            for (index, group) in groups.iter().enumerate() {
                if group.len() < 2 {
                    return Err(Error::Validation(format!(
                        "{kind} group {index} must contain at least two release units"
                    )));
                }
                let mut members = BTreeSet::new();
                for member in group {
                    if !self.release_units.contains_key(member) {
                        return Err(Error::Validation(format!(
                            "{kind} group {index} references unknown release unit {member}"
                        )));
                    }
                    if !members.insert(member) {
                        return Err(Error::Validation(format!(
                            "{kind} group {index} repeats release unit {member}"
                        )));
                    }
                    if let Some(previous) = assigned.insert(member, format!("{kind} group {index}"))
                    {
                        return Err(Error::Validation(format!(
                            "release unit {member} belongs to both {previous} and {kind} group {index}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_dependency_acyclic(&self) -> Result<()> {
        fn visit<'a>(
            id: &'a str,
            release_units: &'a BTreeMap<String, ReleaseUnitConfig>,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err(Error::Validation(format!(
                    "dependency cycle includes release unit {id}"
                )));
            }
            for dependency in &release_units[id].depends_on {
                visit(dependency, release_units, visiting, visited)?;
            }
            visiting.remove(id);
            visited.insert(id);
            Ok(())
        }

        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in self.release_units.keys() {
            visit(id, &self.release_units, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    fn validate_tag_graph(&self, known: &BTreeSet<String>) -> Result<()> {
        let mut edges = BTreeMap::<String, Vec<String>>::new();
        for (release_unit_id, release_unit) in &self.release_units {
            for (tag_id, tag) in &release_unit.tags {
                edges.insert(
                    Self::release_unit_tag_id(release_unit_id, tag_id),
                    tag.tag_after.clone(),
                );
            }
        }
        for (tag_id, tag) in &self.workspace_tags {
            edges.insert(Self::workspace_tag_id(tag_id), tag.tag_after.clone());
        }
        for (id, prerequisites) in &edges {
            let mut unique = BTreeSet::new();
            for prerequisite in prerequisites {
                if prerequisite == id {
                    return Err(Error::Validation(format!(
                        "tag {id} cannot depend on itself"
                    )));
                }
                if !known.contains(prerequisite) {
                    return Err(Error::Validation(format!(
                        "tag {id} depends on unknown tag {prerequisite}"
                    )));
                }
                if !unique.insert(prerequisite) {
                    return Err(Error::Validation(format!(
                        "tag {id} repeats prerequisite {prerequisite}"
                    )));
                }
            }
        }

        fn visit<'a>(
            id: &'a str,
            edges: &'a BTreeMap<String, Vec<String>>,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err(Error::Validation(format!("tag-order cycle includes {id}")));
            }
            for dependency in &edges[id] {
                visit(dependency, edges, visiting, visited)?;
            }
            visiting.remove(id);
            visited.insert(id);
            Ok(())
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in edges.keys() {
            visit(id, &edges, &mut visiting, &mut visited)?;
        }
        Ok(())
    }
}

fn validate_dependencies(
    id: &str,
    release_unit: &ReleaseUnitConfig,
    release_units: &BTreeMap<String, ReleaseUnitConfig>,
) -> Result<()> {
    let mut dependencies = BTreeSet::new();
    for dependency in &release_unit.depends_on {
        if dependency == id {
            return Err(Error::Validation(format!(
                "release unit {id} cannot depend on itself"
            )));
        }
        if !release_units.contains_key(dependency) {
            return Err(Error::Validation(format!(
                "release unit {id} depends on unknown release unit {dependency}"
            )));
        }
        if !dependencies.insert(dependency) {
            return Err(Error::Validation(format!(
                "release unit {id} repeats dependency {dependency}"
            )));
        }
    }
    Ok(())
}

fn validate_package_publishers(id: &str, package_id: &str, package: &PackageConfig) -> Result<()> {
    let scope =
        |publisher: PublisherKind| format!("release unit {id} package {package_id} {publisher}");
    if let Some(npm) = &package.npm {
        validate_optional_identifier(
            npm.token_secret.as_deref(),
            &format!("{} token-secret", scope(PublisherKind::Npm)),
        )?;
        if let Some(targets) = &npm.additional_targets {
            if targets.github.is_none() {
                return Err(Error::Validation(format!(
                    "{} additional-targets must name at least one destination",
                    scope(PublisherKind::Npm)
                )));
            }
        }
    }
    if let Some(cargo) = &package.cargo {
        validate_optional_identifier(
            cargo.token_secret.as_deref(),
            &format!("{} token-secret", scope(PublisherKind::Cargo)),
        )?;
    }
    if let Some(homebrew) = &package.homebrew {
        validate_repository(
            &homebrew.repository,
            &format!("{} repository", scope(PublisherKind::Homebrew)),
        )?;
    }
    if let Some(oci) = &package.oci {
        if oci.dockerhub.is_none() && oci.ghcr.is_none() {
            return Err(Error::Validation(format!(
                "{} must name at least one of dockerhub or ghcr",
                scope(PublisherKind::Oci)
            )));
        }
        if let Some(dockerhub) = &oci.dockerhub {
            let scope = format!("{} dockerhub", scope(PublisherKind::Oci));
            if let Some(repository) = &dockerhub.repository {
                validate_repository(repository, &format!("{scope} repository"))?;
            }
            validate_optional_identifier(
                dockerhub.username_var.as_deref(),
                &format!("{scope} username-var"),
            )?;
            validate_optional_identifier(
                dockerhub.token_secret.as_deref(),
                &format!("{scope} token-secret"),
            )?;
            validate_omit(&dockerhub.omit, &scope)?;
        }
        if let Some(ghcr) = &oci.ghcr {
            let scope = format!("{} ghcr", scope(PublisherKind::Oci));
            if let Some(repository) = &ghcr.repository {
                validate_repository(repository, &format!("{scope} repository"))?;
            }
            validate_omit(&ghcr.omit, &scope)?;
        }
    }
    Ok(())
}

fn validate_omit(omit: &[AttachedComponent], description: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for component in omit {
        if !seen.insert(component) {
            return Err(Error::Validation(format!(
                "{description} repeats omitted component {component}"
            )));
        }
    }
    Ok(())
}

fn validate_optional_identifier(value: Option<&str>, description: &str) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let valid_start = value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_');
    if !valid_start
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(Error::Validation(format!(
            "{description} {value:?} must be a GitHub variable or secret name"
        )));
    }
    Ok(())
}

fn validate_repository(value: &str, description: &str) -> Result<()> {
    let segments = value.split('/').collect::<Vec<_>>();
    let valid = segments.len() == 2
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "_.-".contains(character))
        });
    if !valid {
        return Err(Error::Validation(format!(
            "{description} {value:?} must be owner/name"
        )));
    }
    Ok(())
}

fn validate_workflow_job_id(value: &str, description: &str) -> Result<()> {
    let valid_start = value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_');
    if !valid_start
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(Error::Validation(format!(
            "{description} {value:?} must be a GitHub job identifier"
        )));
    }
    Ok(())
}

/// Whether one release-unit identifier is a workspace identifier.
///
/// Exposed so the publication boundary can be pinned against this rule. The two
/// deliberately differ — this one admits `@` and `/` so a workspace can key a
/// release unit the way a scoped package is named, and a publishing release
/// unit is held to less because its identifier reaches a recipe's scripts and
/// evidence. `executor::names::release_unit` states that side.
#[cfg(test)]
pub(crate) fn validate_release_unit_id(id: &str) -> Result<()> {
    validate_id(id, "release unit")
}

fn validate_id(id: &str, kind: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.@/".contains(character))
    {
        return Err(Error::Validation(format!("invalid {kind} id {id:?}")));
    }
    Ok(())
}

pub(crate) fn validate_detector_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        || !id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || "-_.".contains(character)
        })
    {
        return Err(Error::Validation(format!("invalid detector id {id:?}")));
    }
    Ok(())
}

pub(crate) fn validate_sha256(value: &str, description: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(Error::Validation(format!(
            "{description} must be sha256 followed by 64 lowercase hexadecimal digits"
        )));
    };
    if digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(Error::Validation(format!(
            "{description} must be sha256 followed by 64 lowercase hexadecimal digits"
        )));
    }
    Ok(())
}

fn validate_relative_path(path: &Path, description: &str) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::Validation(format!(
            "{description} must be a non-empty relative path without .."
        )));
    }
    Ok(())
}

/// Validate one exact workspace-relative discovery path.
///
/// A lone `.` names the workspace root directory, which directory-scoped
/// detectors use as a candidate path. Every other path must render exactly as
/// its own normal components so no receipt can carry a glob or a `./` prefix.
pub(crate) fn validate_exact_discovery_path(path: &Path, description: &str) -> Result<()> {
    validate_relative_path(path, description)?;
    let rendered = path.to_string_lossy();
    if rendered == "." {
        return Ok(());
    }
    let normalized = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if rendered.replace('\\', "/") != normalized
        || rendered
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return Err(Error::Validation(format!(
            "{description} must identify one exact workspace-relative path"
        )));
    }
    Ok(())
}

pub(crate) fn validate_tag_template(
    description: &str,
    template: &str,
    allow_id: bool,
) -> Result<()> {
    if template.matches("{version}").count() != 1
        || template.matches("{id}").count() > usize::from(allow_id)
    {
        return Err(Error::Validation(format!(
            "{description} template must contain exactly one {{version}}{}",
            if allow_id {
                " and at most one {id}"
            } else {
                " and no {id}"
            }
        )));
    }
    if template.contains("v{version}") {
        return Err(Error::Validation(format!(
            "{description} template must not prefix versions with v"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
settings:
  internal-dependency-bump: patch
  pre-1-0-bump-mapping: component
workspace-tags:
  release:
    template: '{version}'
release-units:
  library:
    path: packages/library
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  application:
    path: packages/application
    depends-on: [library]
    projections:
      - adapter: json
        file: metadata.json
        pointer: /version
        mode: injected
    tags:
      primary: { role: primary, template: 'application@{version}' }
"#;

    #[test]
    fn parses_complete_contract() {
        let config = Config::from_yaml(VALID).expect("valid config");
        assert_eq!(config.contract, CURRENT_CONTRACT);
        assert_eq!(
            config.settings.pre_1_0_bump_mapping,
            Pre1BumpMapping::Component
        );
        assert_eq!(config.primary_tag("library").expect("primary").0, "primary");
    }

    #[test]
    fn validates_managed_and_exact_excluded_discovery_receipts() {
        let receipts = VALID.replace(
            "release-units:\n",
            "discovery:\n  managed-paths:\n    - detector: npm-package\n      path: packages/library/package.json\n      release-unit: library\n  excluded-paths:\n    - detector: npm-package\n      path: examples/package.json\n      evidence-digest: sha256:0000000000000000000000000000000000000000000000000000000000000000\nrelease-units:\n",
        );
        let config = Config::from_yaml(&receipts).expect("discovery receipts accepted");
        assert_eq!(config.discovery.managed_paths.len(), 1);
        assert_eq!(config.discovery.excluded_paths.len(), 1);

        let unknown = receipts.replace("release-unit: library", "release-unit: missing");
        assert!(Config::from_yaml(&unknown)
            .expect_err("unknown managed target rejected")
            .to_string()
            .contains("unknown release unit missing"));

        let glob = receipts.replace("examples/package.json", "examples/*.json");
        assert!(Config::from_yaml(&glob)
            .expect_err("glob exclusion rejected")
            .to_string()
            .contains("one exact workspace-relative path"));

        let non_canonical = receipts.replace("examples/package.json", "./examples/package.json");
        assert!(Config::from_yaml(&non_canonical)
            .expect_err("non-canonical exact path rejected")
            .to_string()
            .contains("one exact workspace-relative path"));

        // The workspace root is the one directory-scoped path, spelled exactly.
        let root = receipts.replace("path: examples/package.json", "path: \".\"");
        assert_eq!(
            Config::from_yaml(&root)
                .expect("workspace-root exclusion accepted")
                .discovery
                .excluded_paths[0]
                .path,
            Path::new(".")
        );
        for spelling in ["./", ".//", "././.", "./."] {
            let rendered = receipts.replace(
                "path: examples/package.json",
                &format!("path: \"{spelling}\""),
            );
            assert!(
                Config::from_yaml(&rendered)
                    .expect_err("non-canonical workspace-root spelling rejected")
                    .to_string()
                    .contains("one exact workspace-relative path"),
                "{spelling} was accepted"
            );
        }

        let stale_shape = receipts.replace(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "sha256:abcd",
        );
        assert!(Config::from_yaml(&stale_shape)
            .expect_err("invalid exclusion digest rejected")
            .to_string()
            .contains("64 lowercase hexadecimal digits"));
    }

    #[test]
    fn published_config_schema_carries_discovery_receipt_contracts() {
        let schema: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../schemas/config.yml"))
                .expect("config schema parses");
        let discovery = &schema["$defs"]["discovery"];
        assert_eq!(discovery["additionalProperties"].as_bool(), Some(false));
        assert_eq!(
            discovery["properties"]["managed-paths"]["items"]["required"]
                .as_sequence()
                .expect("managed receipt required fields")
                .len(),
            3
        );
        assert_eq!(
            discovery["properties"]["excluded-paths"]["items"]["properties"]["evidence-digest"]
                ["pattern"]
                .as_str(),
            Some("^sha256:[0-9a-f]{64}$")
        );
    }

    #[test]
    fn validates_fixed_and_linked_membership() {
        let invalid = VALID.replace(
            "release-units:\n",
            "fixed: [[library, application]]\nlinked: [[library, application]]\nrelease-units:\n",
        );
        let error = Config::from_yaml(&invalid).expect_err("overlap rejected");
        assert!(error.to_string().contains("belongs to both"));
    }

    #[test]
    fn permits_tag_only_release_units() {
        let valid = VALID.replace(
            "    projections:\n      - { adapter: npm, file: package.json, mode: committed }\n    tags:",
            "    tags:",
        );
        Config::from_yaml(&valid).expect("tag-only release unit accepted");
    }

    #[test]
    fn rejects_obsolete_packages_key() {
        let obsolete = VALID.replace("release-units:", "packages:");
        let error = Config::from_yaml(&obsolete).expect_err("obsolete public key rejected");
        assert!(error.to_string().contains("unknown field `packages`"));
    }

    #[test]
    fn rejects_missing_or_duplicate_primary_tags() {
        let missing = VALID.replace(
            "role: primary, template: '{id}@{version}'",
            "role: projection, template: '{id}@{version}'",
        );
        assert!(Config::from_yaml(&missing)
            .expect_err("missing primary rejected")
            .to_string()
            .contains("exactly one primary"));
        let duplicate = VALID.replace(
            "primary: { role: primary, template: '{id}@{version}' }",
            "primary: { role: primary, template: '{id}@{version}' }\n      second: { role: primary, template: 'second@{version}' }",
        );
        assert!(Config::from_yaml(&duplicate)
            .expect_err("duplicate primary rejected")
            .to_string()
            .contains("exactly one primary"));
    }

    #[test]
    fn rejects_unknown_and_cyclic_tag_prerequisites() {
        let unknown = VALID.replace(
            "template: '{version}'",
            "template: '{version}'\n    tag-after: [workspace/missing]",
        );
        assert!(Config::from_yaml(&unknown)
            .expect_err("unknown tag rejected")
            .to_string()
            .contains("unknown tag"));
        let cyclic = VALID
            .replace("template: '{version}'", "template: '{version}'\n    tag-after: [release-unit/library/primary]")
            .replace(
                "primary: { role: primary, template: '{id}@{version}' }",
                "primary: { role: primary, template: '{id}@{version}', tag-after: [workspace/release] }",
            );
        assert!(Config::from_yaml(&cyclic)
            .expect_err("tag cycle rejected")
            .to_string()
            .contains("tag-order cycle"));
    }

    const GITHUB: &str = r#"github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
"#;

    fn with_github(extra: &str) -> String {
        VALID.replace(
            "release-units:\n",
            &format!("{GITHUB}{extra}release-units:\n"),
        )
    }

    #[test]
    fn requires_github_executor_for_configured_publishers() {
        let without = VALID.replace(
            "    path: packages/library\n",
            "    path: packages/library\n    packages:\n      library:\n        path: .\n        npm: {}\n",
        );
        assert!(Config::from_yaml(&without)
            .expect_err("publisher without executor rejected")
            .to_string()
            .contains("requires GitHub executor configuration"));

        let with = with_github("").replace(
            "    path: packages/library\n",
            "    path: packages/library\n    packages:\n      library:\n        path: .\n        npm: {}\n",
        );
        let config = Config::from_yaml(&with).expect("publisher with executor accepted");
        assert_eq!(
            config.release_units["library"].publishers(),
            vec![PublisherKind::Npm]
        );
    }

    #[test]
    fn rejects_contract_one_before_typed_publisher_parsing() {
        let legacy = VALID
            .replace("contract: contract-2", "contract: contract-1")
            .replace(
                "    path: packages/library\n",
                "    path: packages/library\n    npm: {}\n",
            );
        let error = Config::from_yaml(&legacy).expect_err("contract-1 is migrated forward");
        let message = error.to_string();
        assert!(message.contains("migrate to contract-2"), "{message}");
        assert!(
            message.contains("release-units.<unit>.packages")
                && message.contains("moving each publisher mapping beneath the package"),
            "{message}"
        );
        assert!(!message.contains("unknown field"), "{message}");
    }

    #[test]
    fn rejects_publisher_properties_on_a_release_unit() {
        let direct = with_github("").replace(
            "    path: packages/library\n",
            "    path: packages/library\n    npm: {}\n",
        );
        let error = Config::from_yaml(&direct).expect_err("release-unit publisher rejected");
        assert!(error.to_string().contains("unknown field `npm`"), "{error}");
    }

    #[test]
    fn round_trips_direct_publisher_properties() {
        let text = with_github("").replace(
            "    path: packages/application\n",
            r#"    path: packages/application
    packages:
      application:
        path: .
        homebrew: { repository: example-org/homebrew-tap }
        oci:
          ghcr: { omit: [ signature ] }
          dockerhub: { repository: example-org/example-image, username-var: EXAMPLE_USER }
"#,
        );
        let config = Config::from_yaml(&text).expect("publisher properties accepted");
        let application = &config.release_units["application"];
        assert_eq!(
            application.publishers(),
            vec![PublisherKind::Homebrew, PublisherKind::Oci]
        );
        let oci = application.oci().expect("oci publisher");
        assert_eq!(
            oci.ghcr.as_ref().expect("ghcr target").omit,
            vec![AttachedComponent::Signature]
        );
        let reparsed = Config::from_yaml(&config.to_yaml().expect("serializes"))
            .expect("serialized config reparses");
        assert_eq!(reparsed, config);
    }

    #[test]
    fn rejects_publisher_shapes_that_carry_no_destination() {
        let empty_oci = with_github("").replace(
            "    path: packages/library\n",
            "    path: packages/library\n    packages:\n      library:\n        path: .\n        oci: {}\n",
        );
        assert!(Config::from_yaml(&empty_oci)
            .expect_err("empty oci rejected")
            .to_string()
            .contains("at least one of dockerhub or ghcr"));

        let empty_additional = with_github("").replace(
            "    path: packages/library\n",
            "    path: packages/library\n    packages:\n      library:\n        path: .\n        npm: { additional-targets: {} }\n",
        );
        assert!(Config::from_yaml(&empty_additional)
            .expect_err("empty additional targets rejected")
            .to_string()
            .contains("at least one destination"));
    }

    #[test]
    fn rejects_secret_names_that_are_not_identifiers() {
        let invalid = with_github("").replace(
            "    path: packages/library\n",
            "    path: packages/library\n    packages:\n      library:\n        path: .\n        cargo: { token-secret: 'not a name' }\n",
        );
        assert!(Config::from_yaml(&invalid)
            .expect_err("non-identifier secret name rejected")
            .to_string()
            .contains("must be a GitHub variable or secret name"));
    }

    #[test]
    fn normalizes_scalar_and_explicit_prefixes() {
        let default = Config::from_yaml(&with_github("")).expect("default prefix accepted");
        let namespaces = default
            .github
            .as_ref()
            .expect("github config")
            .namespaces()
            .expect("default namespaces");
        assert_eq!(namespaces.job, DEFAULT_JOB_PREFIX);
        assert_eq!(namespaces.envvar, DEFAULT_ENVVAR_PREFIX);
        assert_eq!(namespaces.environment, "intentional-release");

        let scalar = Config::from_yaml(&with_github("").replace(
            "  workflows:\n",
            "  prefix: releaseAutomation\n  workflows:\n",
        ))
        .expect("scalar prefix accepted");
        let namespaces = scalar
            .github
            .as_ref()
            .expect("github config")
            .namespaces()
            .expect("scalar namespaces");
        assert_eq!(namespaces.job, "release_automation_");
        assert_eq!(namespaces.envvar, "RELEASE_AUTOMATION_");
        assert_eq!(namespaces.environment, "release-automation-release");

        let explicit = Config::from_yaml(&with_github("").replace(
            "  workflows:\n",
            "  prefix: { job: managed-slice, envvar: MANAGED }\n  workflows:\n",
        ))
        .expect("explicit prefix accepted");
        let namespaces = explicit
            .github
            .as_ref()
            .expect("github config")
            .namespaces()
            .expect("explicit namespaces");
        assert_eq!(namespaces.job, "managed-slice_");
        assert_eq!(namespaces.envvar, "MANAGED_");
        assert_eq!(namespaces.environment, "managed-slice-release");
    }

    #[test]
    fn rejects_prefixes_that_cannot_form_identifiers() {
        let invalid = with_github("").replace("  workflows:\n", "  prefix: '1234'\n  workflows:\n");
        assert!(Config::from_yaml(&invalid)
            .expect_err("numeric prefix rejected")
            .to_string()
            .contains("must normalize to an identifier"));
    }

    #[test]
    fn rejects_gates_that_collide_with_reserved_jobs() {
        let invalid = with_github("").replace(
            "    release: { path: .github/workflows/release.yml }",
            "    release: { path: .github/workflows/release.yml, gates: [ intentional_prepare ] }",
        );
        assert!(Config::from_yaml(&invalid)
            .expect_err("reserved gate rejected")
            .to_string()
            .contains("uses the reserved job prefix"));

        let valid = with_github("").replace(
            "    release: { path: .github/workflows/release.yml }",
            "    release: { path: .github/workflows/release.yml, gates: [ candidate_check ] }",
        );
        let config = Config::from_yaml(&valid).expect("repository-owned gate accepted");
        assert_eq!(
            config
                .github
                .expect("github config")
                .workflows
                .release
                .gates,
            vec!["candidate_check".to_owned()]
        );
    }

    #[test]
    fn rejects_one_workflow_file_serving_both_executor_roles() {
        let invalid = with_github("").replace(
            ".github/workflows/publish.yml",
            ".github/workflows/release.yml",
        );
        assert!(Config::from_yaml(&invalid)
            .expect_err("shared workflow rejected")
            .to_string()
            .contains("must be distinct files"));
    }

    #[test]
    fn published_config_schema_carries_executor_and_publisher_contracts() {
        let schema: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../schemas/config.yml"))
                .expect("config schema parses");
        let github = &schema["$defs"]["github"];
        assert_eq!(github["additionalProperties"].as_bool(), Some(false));
        assert_eq!(github["required"].as_sequence().expect("required").len(), 1);
        let package = &schema["$defs"]["package"]["properties"];
        for publisher in ["npm", "cargo", "homebrew", "rpm", "apt", "aur", "oci"] {
            assert!(
                package[publisher].is_mapping(),
                "schema declares the {publisher} publisher"
            );
        }
        assert_eq!(
            schema["$defs"]["attached-component"]["enum"]
                .as_sequence()
                .expect("attached components")
                .len(),
            3
        );
    }

    #[test]
    fn rejects_versions_and_legacy_global_tag() {
        let versioned = VALID.replace(
            "path: packages/library",
            "path: packages/library\n    version: 1.2.3",
        );
        assert!(Config::from_yaml(&versioned).is_err());
        let legacy = VALID.replace(
            "internal-dependency-bump: patch",
            "internal-dependency-bump: patch\n  global-tag: true",
        );
        assert!(Config::from_yaml(&legacy).is_err());
    }
}
