// ---
// relationships:
//   implements: github-release-executor
// ---

//! GitHub executor configuration, recipe selection, initialization, and conformance.

pub mod check;
pub mod goreleaser;
pub mod init;
mod names;
pub mod recipe;
pub(crate) mod steps;
pub mod workflow;

#[cfg(any(test, feature = "test-support"))]
mod credential_derivation;

pub use check::{check_executor, ExecutorCheck};
pub use init::{
    initialize_executor, CandidateKind, Choice, ExecutorCandidate, ExecutorInitPlan,
    ExecutorInitResult, ExecutorInitState, ACCEPT_CHOICE, DECLINE_CHOICE, EXECUTOR_INIT_PLAN_PATH,
    EXECUTOR_INIT_PLAN_SCHEMA,
};
pub use workflow::{
    compare_configured_workflow, compare_workflow, ComparisonStatus, WorkflowComparison,
    WorkflowDiagnostic, OWNERSHIP_SENTINEL, WORKFLOW_CONTRACT, WORKFLOW_DIFF_SCHEMA,
};

pub use recipe::{
    capability_set, catalog, configured_publication_identities, derive_capabilities, recipes_for,
    resolve_publications, select_publications, Capability, CapabilityEvidence, Packager,
    PublicationSelection, Recipe, SelectedPublication, PRIMARY_TARGET,
};

/// Workspace and workflow-derivation fixtures shared by executor tests.
///
/// Tests in this crate and the command-line crate need self-deleting workspaces
/// with the same ownership and uniqueness contract. A second copy is a second
/// thing to keep in step with that contract, so the fixture is published under
/// the `test-support` feature rather than transcribed per crate. The feature is
/// enabled only by dev-dependencies, so nothing here reaches a released binary.
#[cfg(any(test, feature = "test-support"))]
pub mod fixture {
    use crate::config::Config;
    use crate::executor::recipe::{catalog, Capability, Recipe, PRIMARY_TARGET};
    use crate::model::PublisherKind;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    /// Render the managed publish job kinds and their direct dependency graph as Mermaid.
    ///
    /// Multiple derivations supply conditional kinds and edges without turning
    /// any one repository configuration into the public diagram. The checked-in
    /// source can therefore be compared with this projection instead of carrying
    /// a second hand-maintained graph roster.
    #[must_use]
    pub fn publish_workflow_kind_mermaid(workflows: &[String]) -> String {
        let mut kinds = BTreeMap::<&str, bool>::new();
        let mut edges = BTreeSet::<(&str, &str)>::new();

        for workflow in workflows {
            let document: serde_yaml::Value =
                serde_yaml::from_str(workflow).expect("derived publish workflow parses");
            let jobs = document["jobs"]
                .as_mapping()
                .expect("derived publish workflow has jobs");
            let job_kinds = jobs
                .iter()
                .filter_map(|(id, body)| {
                    let id = id.as_str().expect("publish job id is text");
                    publish_job_kind(id, body).map(|kind| (id, kind))
                })
                .collect::<BTreeMap<_, _>>();

            for (id, kind) in &job_kinds {
                let body = jobs
                    .get(serde_yaml::Value::String((*id).to_owned()))
                    .expect("managed publish job remains in its derivation");
                let protected = body.get("environment").is_some();
                if let Some(existing) = kinds.insert(kind, protected) {
                    assert_eq!(
                        existing, protected,
                        "publish job kind {kind} has one environment boundary"
                    );
                }
                for need in job_needs(body) {
                    if let Some(need_kind) = job_kinds.get(need) {
                        edges.insert((need_kind, kind));
                    }
                }
            }
        }

        let mut source = String::from(
            "%% Generated from derived publish workflows; do not edit by hand.\n\
%%{init: {\"theme\":\"dark\",\"htmlLabels\":false,\"deterministicIds\":true,\"deterministicIDSeed\":\"intentional-publish-workflow\",\"flowchart\":{\"curve\":\"basis\"},\"themeVariables\":{\"background\":\"#24292e\",\"primaryTextColor\":\"#f0f6fc\",\"lineColor\":\"#79c0ff\",\"fontFamily\":\"DejaVu Sans\"}}}%%\n\
flowchart LR\n\
  classDef standard fill:#30363d,stroke:#8b949e,color:#f0f6fc\n\
  classDef protected fill:#3d2f1f,stroke:#d29922,color:#f0f6fc\n\
  classDef terminal fill:#3b2344,stroke:#bc8cff,color:#f0f6fc\n",
        );
        for kind in kinds.keys() {
            source.push_str(&format!(
                "  {}[\"{}\"]\n",
                mermaid_kind_id(kind),
                publish_kind_label(kind)
            ));
        }
        for (need, job) in &edges {
            source.push_str(&format!(
                "  {} --> {}\n",
                mermaid_kind_id(need),
                mermaid_kind_id(job)
            ));
        }

        let standard = kinds
            .iter()
            .filter(|(_, protected)| !**protected)
            .map(|(kind, _)| mermaid_kind_id(kind))
            .collect::<Vec<_>>();
        let protected = kinds
            .iter()
            .filter(|(_, protected)| **protected)
            .map(|(kind, _)| mermaid_kind_id(kind))
            .collect::<Vec<_>>();
        source.push_str(&format!("  class {} standard\n", standard.join(",")));
        source.push_str(&format!("  class {} protected\n", protected.join(",")));
        if kinds.contains_key("close") {
            source.push_str("  class close terminal\n");
        }
        source
    }

    fn publish_job_kind(id: &str, body: &serde_yaml::Value) -> Option<&'static str> {
        let managed = body["steps"].as_sequence().is_some_and(|steps| {
            steps
                .iter()
                .any(|step| step["id"].as_str() == Some(crate::executor::OWNERSHIP_SENTINEL))
        });
        if !managed {
            return None;
        }
        Some(if id.ends_with("verify_tag") {
            "verify-tag"
        } else if id.contains("_build_") {
            "build"
        } else if id.ends_with("tag_before_publication") {
            "phase-before"
        } else if id.ends_with("upload_deliverables") {
            "upload"
        } else if id.contains("_publish_") {
            "publisher"
        } else if id.contains("_verify_") || id.contains("_retrieve_") {
            "verifier"
        } else if id.ends_with("tag_after_publication") {
            "phase-after"
        } else if id.ends_with("assemble_evidence") {
            "assemble"
        } else if id.ends_with("close_release") {
            "close"
        } else {
            panic!("managed publish job {id} has no diagram kind")
        })
    }

    fn job_needs(body: &serde_yaml::Value) -> Vec<&str> {
        match body.get("needs") {
            None | Some(serde_yaml::Value::Null) => Vec::new(),
            Some(serde_yaml::Value::String(need)) => vec![need],
            Some(serde_yaml::Value::Sequence(needs)) => needs
                .iter()
                .map(|need| need.as_str().expect("managed job need is text"))
                .collect(),
            Some(other) => panic!("managed publish job has unsupported needs {other:?}"),
        }
    }

    fn mermaid_kind_id(kind: &str) -> &str {
        match kind {
            "verify-tag" => "verify_tag",
            "build" => "build",
            "phase-before" => "phase_before",
            "upload" => "upload",
            "publisher" => "publisher",
            "verifier" => "verifier",
            "phase-after" => "phase_after",
            "assemble" => "assemble",
            "close" => "close",
            other => panic!("publish job kind {other} has no Mermaid id"),
        }
    }

    fn publish_kind_label(kind: &str) -> &str {
        match kind {
            "verify-tag" => "verify_tag",
            "build" => "build_*",
            "phase-before" => "tag_before_publication",
            "upload" => "upload_deliverables",
            "publisher" => "publish_*",
            "verifier" => "verify_* / retrieve_*",
            "phase-after" => "tag_after_publication",
            "assemble" => "assemble_evidence",
            "close" => "close_release",
            other => panic!("publish job kind {other} has no diagram label"),
        }
    }

    /// Directory name for one fixture workspace.
    ///
    /// The label says what the workspace is for; it is not an identity. Tests
    /// reach a label through shared helpers, so one label names many workspaces
    /// within one binary, and two of them can be created inside a single clock
    /// tick. Neither the label nor the instant is therefore allowed to carry
    /// uniqueness: a process-wide sequence number does, and it is exact rather
    /// than probable.
    fn workspace_directory_name(label: &str, instant: u128) -> String {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        format!(
            "intentional-executor-{label}-{}-{instant}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    /// Self-deleting workspace directory used by executor tests.
    pub struct Workspace(PathBuf);

    impl Workspace {
        #[must_use]
        pub fn new(label: &str) -> Self {
            let instant = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            Self::at(std::env::temp_dir().join(workspace_directory_name(label, instant)))
        }

        /// Claim one directory exclusively.
        ///
        /// `create_dir` refuses a directory that already exists, so a name two
        /// workspaces both derived is a named panic rather than two fixtures
        /// silently sharing a root and deleting it from under each other.
        fn at(path: PathBuf) -> Self {
            std::fs::create_dir(&path).unwrap_or_else(|error| {
                panic!("create workspace {}: {error}", path.display());
            });
            Self(path)
        }

        #[must_use]
        pub fn root(&self) -> &Path {
            &self.0
        }

        pub fn write(&self, relative: &str, contents: &str) -> &Self {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent directory"))
                .expect("create parent directory");
            std::fs::write(&path, contents).expect("write fixture file");
            self
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Executor configuration that derives both managed workflows.
    ///
    /// One release unit with one publishable adapter is the smallest
    /// configuration that still derives every managed job: preparation, the
    /// authority transition, tag verification, one subject build, both phase
    /// tags, one publisher, evidence assembly, and closure.
    pub const MANAGED_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    packages:
      package:
        path: .
        cargo: { registry: {} }
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
      staged: { role: projection, template: '{id}/staged@{version}', require-phase: before-publication }
"#;

    /// Repository-owned workflow the managed slices are spliced into.
    const REPOSITORY_WORKFLOW: &str = "name: managed\n\non:\n  workflow_dispatch:\n\njobs:\n  repository_job:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n";

    /// A representative workspace both managed workflow roles derive from.
    #[must_use]
    pub fn managed_workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace
            .write(".intentional/config.yml", MANAGED_CONFIG)
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
            )
            .write(".github/workflows/release.yml", REPOSITORY_WORKFLOW)
            .write(".github/workflows/publish.yml", REPOSITORY_WORKFLOW);
        workspace
    }

    /// Reconcile that workspace and return what each role's workflow became.
    ///
    /// The workflows are derived rather than transcribed, so every caller reads
    /// exactly what a repository would receive from this build.
    #[must_use]
    pub fn derived_workflows(label: &str) -> Vec<(crate::config::WorkflowRole, String)> {
        let workspace = managed_workspace(label);
        derive_workflows_under(workspace.root())
    }

    /// Derive workflows from every maintained publication recipe.
    ///
    /// The recipe catalog is the source of the fixture's publication set. A
    /// catalog entry added later therefore changes this workspace without a
    /// second roster having to remember it.
    #[must_use]
    pub fn derived_recipe_workflows(label: &str) -> Vec<(crate::config::WorkflowRole, String)> {
        let workspace = recipe_fixture_workspace(label);
        derive_workflows_under(workspace.root())
    }

    /// Derive the recipe workflows under a non-default managed namespace.
    #[must_use]
    pub fn prefixed_derived_recipe_workflows(
        label: &str,
        prefix: &str,
    ) -> (String, Vec<(crate::config::WorkflowRole, String)>) {
        let workspace = recipe_fixture_workspace(label);
        let path = workspace.root().join(".intentional/config.yml");
        let config = std::fs::read_to_string(&path).expect("recipe fixture configuration reads");
        let config = config.replacen("github:\n", &format!("github:\n  prefix: {prefix}\n"), 1);
        workspace.write(".intentional/config.yml", &config);
        let action = RECIPE_DELIVERY_ACTION.replace("intentional-", &format!("{prefix}-"));
        workspace.write(".github/actions/deliver/action.yml", &action);
        let job = Config::load(workspace.root())
            .expect("recipe fixture configuration loads")
            .github
            .expect("recipe fixture enables GitHub execution")
            .namespaces()
            .expect("recipe fixture namespace resolves")
            .job;
        (job, derive_workflows_under(workspace.root()))
    }

    fn recipe_fixture_workspace(label: &str) -> Workspace {
        if label == "usage-claims-credential-populations" {
            credential_population_workspace(label)
        } else {
            recipe_workspace(label)
        }
    }

    /// Exhaustive recipe workspace plus the alternate Cargo registry partition whose
    /// emission uses stored token authentication rather than crates.io trusted publishing.
    fn credential_population_workspace(label: &str) -> Workspace {
        let workspace = recipe_workspace(label);
        workspace
            .write(
                ".cargo/config.toml",
                "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
            )
            .write(
                "rust-crate/Cargo.toml",
                "[package]\nname = \"sample-crate\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n\n[[bin]]\nname = \"sample-tool\"\npath = \"src/main.rs\"\n",
            );
        workspace
    }

    /// Catalog entries for which workflow derivation owns complete shell steps.
    #[must_use]
    pub fn derived_recipes() -> Vec<Recipe> {
        catalog()
            .iter()
            .copied()
            .filter(|recipe| super::steps::recipe_is_derived(recipe.packager, recipe.publisher))
            .collect()
    }

    /// Catalog entries workflow derivation refuses rather than deriving steps for.
    ///
    /// The complement of [`derived_recipes`] over the same catalog, through the
    /// negation of the same predicate. That the two sets sum to [`catalog`] is
    /// therefore a property of how these helpers are written rather than
    /// anything about derivation: it holds for any predicate and survives every
    /// change to `recipe_is_derived`. Do not read a length comparison between
    /// them as coverage.
    ///
    /// What this is for is naming the set a caller must account for. A recipe
    /// that stops deriving appears here, and a caller comparing this set against
    /// the recipes it has refusal coverage for learns that it now has an
    /// uncovered one.
    #[must_use]
    pub fn underived_recipes() -> Vec<Recipe> {
        catalog()
            .iter()
            .copied()
            .filter(|recipe| !super::steps::recipe_is_derived(recipe.packager, recipe.publisher))
            .collect()
    }

    /// Publisher and target identities produced by an exhaustive package declaration.
    ///
    /// The maintained catalog supplies the product-shaped declaration, while
    /// `configured_targets` remains the only code that answers which configured
    /// destinations that declaration produces.
    #[must_use]
    pub fn configured_target_identities() -> Vec<(PublisherKind, String)> {
        let recipes = catalog().iter().collect::<Vec<_>>();
        let source = format!(
            "contract: contract-2\ngithub:\n  workflows:\n    release: {{ path: .github/workflows/release.yml }}\n    publish: {{ path: .github/workflows/publish.yml }}\nrelease-units:\n  component:\n    path: component\n{}    tags:\n      release: {{ role: primary, template: 'component@{{version}}' }}\n",
            publisher_config(&recipes)
        );
        let config = crate::config::Config::from_yaml(&source).expect("catalog config is valid");
        let package = &config.release_units["component"].packages["package"];
        super::recipe::configured_target_identities(package)
    }

    /// Stored credentials the authority transition reads before minting repository-write tokens.
    #[must_use]
    pub fn long_lived_repository_write_credentials(
    ) -> Vec<(super::recipe::StoredCredentialKind, String)> {
        let workflows = derived_recipe_workflows("usage-claims-long-lived-credentials");
        super::credential_derivation::long_lived_repository_write_credentials(&workflows)
    }

    /// Reader-facing labels for routes that authenticate every publication with a stored credential.
    #[must_use]
    pub fn standing_credential_usage_labels() -> Vec<String> {
        let workflows = derived_recipe_workflows("usage-claims-credential-populations");
        super::credential_derivation::standing_credential_usage_labels(&workflows)
    }

    /// Publication routes whose emitted bodies implement registry trusted publishing bootstrap.
    #[must_use]
    pub fn trusted_publishing_bootstrap_route_count() -> usize {
        let workflows = derived_recipe_workflows("usage-claims-bootstrap-routes");
        super::credential_derivation::trusted_publishing_bootstrap_route_count(&workflows)
    }

    /// Managed job identifiers in the derived release workflow.
    #[must_use]
    pub fn release_managed_job_ids() -> Vec<String> {
        let workflows = derived_workflows("usage-claims-release-jobs");
        super::credential_derivation::release_managed_job_ids(&workflows)
    }

    /// Repository setting names derived from the default executor prefixes.
    #[must_use]
    pub fn prefix_derived_repository_settings(
    ) -> super::credential_derivation::PrefixDerivedRepositorySettings {
        let workspace = managed_workspace("usage-claims-prefix-settings");
        let config = Config::load(workspace.root()).expect("fixture configuration loads");
        let github = config.github.expect("fixture enables GitHub execution");
        let namespaces = github.namespaces().expect("fixture namespaces resolve");
        super::credential_derivation::prefix_derived_repository_settings(&namespaces)
    }

    /// GitHub ruleset tag namespace patterns from the exhaustive recipe fixture.
    #[must_use]
    pub fn managed_release_tag_namespace_patterns() -> Vec<String> {
        let workspace = recipe_workspace("usage-claims-tag-namespaces");
        Config::load(workspace.root())
            .expect("fixture configuration loads")
            .managed_release_tag_namespace_patterns()
    }

    /// Publication identities the exhaustive recipe fixture resolves.
    #[must_use]
    pub fn resolved_publication_identities() -> Vec<String> {
        let workspace = recipe_workspace("usage-claims-publications");
        let config = Config::load(workspace.root()).expect("fixture configuration loads");
        let expected = super::recipe::configured_publication_identities(&config);
        let selection = super::recipe::resolve_publications(workspace.root(), &config)
            .expect("fixture publications resolve");
        assert!(
            selection.diagnostics.is_empty(),
            "the exhaustive fixture resolves every configured publication: {:?}",
            selection.diagnostics
        );
        let mut identities = selection
            .selected
            .iter()
            .map(super::recipe::SelectedPublication::identity)
            .collect::<Vec<_>>();
        identities.sort();
        assert_eq!(
            identities, expected,
            "resolved publications match every configured publication identity"
        );
        identities
    }

    /// Workspace whose release units are generated from the maintained catalog.
    fn recipe_workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        let mut config = String::from(
            "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-2\nworkspace-tags:\n  release:\n    template: '{version}'\ngithub:\n  workflows:\n    release: { path: .github/workflows/release.yml }\n    publish: { path: .github/workflows/publish.yml }\nrelease-units:\n",
        );
        let derived = derived_recipes();
        for capability in Capability::ALL {
            let recipes = derived
                .iter()
                .filter(|recipe| recipe.capability == capability)
                .collect::<Vec<_>>();
            assert!(
                !recipes.is_empty(),
                "the canonical capability {capability} has a maintained recipe"
            );
            let id = capability.as_str();
            config.push_str(&format!("  {id}:\n    path: {id}\n"));
            config.push_str(&publisher_config(&recipes));
            config.push_str(&format!(
                "    tags:\n      staged:\n        role: primary\n        template: '{id}/staged@{{version}}'\n        require-phase: before-publication\n      published:\n        role: projection\n        template: '{id}/published@{{version}}'\n        require-phase: after-publication\n"
            ));
            write_capability(&workspace, capability);
        }
        workspace
            .write(".intentional/config.yml", &config)
            .write(".github/workflows/release.yml", REPOSITORY_WORKFLOW)
            .write(".github/workflows/publish.yml", REPOSITORY_WORKFLOW)
            .write(".github/actions/deliver/action.yml", RECIPE_DELIVERY_ACTION);
        workspace
    }

    /// Publisher declarations projected from one capability's catalog entries.
    fn publisher_config(recipes: &[&crate::executor::recipe::Recipe]) -> String {
        let publishers = recipes
            .iter()
            .map(|recipe| recipe.publisher)
            .collect::<BTreeSet<_>>();
        let mut configured = String::new();
        for publisher in publishers {
            let targets = recipes
                .iter()
                .filter(|recipe| recipe.publisher == publisher)
                .map(|recipe| recipe.target)
                .collect::<BTreeSet<_>>();
            match publisher {
                PublisherKind::Npm => {
                    assert!(
                        targets
                            .iter()
                            .all(|target| matches!(*target, PRIMARY_TARGET | "github")),
                        "the npm fixture knows every catalog target: {targets:?}"
                    );
                    if targets.contains("github") {
                        configured.push_str("    npm:\n      npmjs: {}\n      github: {}\n");
                    } else {
                        configured.push_str("    npm: { npmjs: {} }\n");
                    }
                }
                PublisherKind::Cargo => {
                    assert!(
                        targets.len() == 1 && targets.contains(PRIMARY_TARGET),
                        "the cargo fixture knows every catalog target: {targets:?}"
                    );
                    configured.push_str("    cargo: { registry: {} }\n");
                }
                PublisherKind::Homebrew => {
                    assert!(
                        targets.len() == 1 && targets.contains(PRIMARY_TARGET),
                        "the Homebrew fixture knows every catalog target: {targets:?}"
                    );
                    configured.push_str("    homebrew: { repository: sample-owner/sample-tap }\n")
                }
                PublisherKind::Rpm => {
                    assert!(
                        targets.len() == 1 && targets.contains(PRIMARY_TARGET),
                        "the RPM fixture knows every catalog target: {targets:?}"
                    );
                    configured.push_str("    rpm:\n      delivery-action: .github/actions/deliver\n      base-url: https://packages.invalid/rpm\n      public-signing-key-url: https://packages.invalid/key.asc\n      observation-deadline: 47\n      channel: stable\n      with: {}\n");
                }
                PublisherKind::Apt => {
                    assert!(
                        targets.len() == 1 && targets.contains(PRIMARY_TARGET),
                        "the APT fixture knows every catalog target: {targets:?}"
                    );
                    configured.push_str("    apt:\n      delivery-action: .github/actions/deliver\n      base-url: https://packages.invalid/apt\n      public-signing-key-url: https://packages.invalid/key.asc\n      observation-deadline: 47\n      suite: current\n      component: main\n      with: {}\n");
                }
                PublisherKind::Aur => {
                    assert!(
                        targets.len() == 1 && targets.contains(PRIMARY_TARGET),
                        "the AUR fixture knows every catalog target: {targets:?}"
                    );
                    configured.push_str("    aur: {}\n");
                }
                PublisherKind::Oci => {
                    configured.push_str("    oci:\n");
                    for target in targets {
                        match target {
                            "dockerhub" => configured.push_str(
                                "      dockerhub: { repository: sample-owner/sample-image }\n",
                            ),
                            "ghcr" => configured.push_str("      ghcr: {}\n"),
                            other => panic!("the OCI fixture knows every catalog target: {other}"),
                        }
                    }
                }
            }
        }
        let configured = configured
            .lines()
            .map(|line| format!("    {line}\n"))
            .collect::<String>();
        format!("    packages:\n      package:\n        path: .\n{configured}")
    }

    /// Native evidence from which one canonical capability is derived.
    fn write_capability(workspace: &Workspace, capability: Capability) {
        let root = capability.as_str();
        match capability {
            Capability::NodePackage => {
                workspace.write(
                    &format!("{root}/package.json"),
                    r#"{"name":"@sample-owner/sample-package","version":"1.0.0"}"#,
                );
            }
            Capability::RustCrate => {
                workspace
                    .write(
                        &format!("{root}/Cargo.toml"),
                        "[package]\nname = \"sample-crate\"\nversion = \"1.0.0\"\n\n[[bin]]\nname = \"sample-tool\"\npath = \"src/main.rs\"\n",
                    )
                    .write(&format!("{root}/src/main.rs"), "fn main() {}\n");
            }
            Capability::GoApplication => {
                workspace
                    .write(
                        &format!("{root}/go.mod"),
                        "module example.invalid/sample-application\n",
                    )
                    .write(
                        &format!("{root}/cmd/sample-application/main.go"),
                        "package main\n\nfunc main() {}\n",
                    )
                    .write(&format!("{root}/.goreleaser.yaml"), RECIPE_GORELEASER);
            }
            Capability::RunnableImage => {
                workspace.write(
                    &format!("{root}/Dockerfile"),
                    "FROM scratch\nLABEL org.opencontainers.image.title=\"sample-image\"\n",
                );
            }
            Capability::DevContainerFeature => {
                workspace.write(
                    &format!("{root}/devcontainer-feature.json"),
                    r#"{"id":"sample-feature","version":"1.0.0"}"#,
                );
            }
        }
    }

    /// Native configuration carrying every GoReleaser destination recipe reads.
    const RECIPE_GORELEASER: &str = r#"version: 2
project_name: sample-application
builds:
  - main: ./cmd/sample-application
brews:
  - repository: { owner: sample-owner, name: sample-tap }
nfpms:
  - formats: [ rpm, deb, archlinux ]
aur:
  - name: sample-application-bin
"#;

    const RECIPE_DELIVERY_ACTION: &str = "name: delivery\ninputs:\n  intentional-package-path: {}\n  intentional-format: {}\n  intentional-name: {}\n  intentional-version: {}\n  intentional-architecture: {}\n  intentional-digest: {}\n  intentional-rpm-channel: {}\n  intentional-apt-suite: {}\n  intentional-apt-component: {}\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: 'true'\n";

    /// Derive both managed workflows in a workspace someone else owns.
    ///
    /// Every way a derivation can fail arrives at one panic, so the diagnostic
    /// is wired once and one test reaches it. Reporting each failure at its own
    /// site left the wiring provable nowhere.
    fn derive_workflows_under(root: &Path) -> Vec<(crate::config::WorkflowRole, String)> {
        crate::config::WorkflowRole::ALL
            .into_iter()
            .map(|role| {
                let derived = derive_role(root, role)
                    .unwrap_or_else(|error| panic!("{}", derivation_failure(root, role, &error)));
                (role, derived)
            })
            .collect()
    }

    /// Materialize one catalog-wide role for structural test inspection.
    fn derive_role(root: &Path, role: crate::config::WorkflowRole) -> crate::Result<String> {
        super::workflow::materialize_contract_for_structural_test(root, role)
    }

    /// Why one role's derivation did not finish.
    ///
    /// The two failures a derivation can produce read identically — a missing
    /// file under a temporary path — but they have opposite causes. A root that
    /// is still present means the contract or the workspace contents are wrong.
    /// A root that has gone missing means the fixture directory was deleted
    /// while the derivation was running, which is a fixture-lifetime defect and
    /// not a contract defect. Reporting which one held is what stops the next
    /// reader from investigating the wrong one.
    fn derivation_failure(
        root: &Path,
        role: crate::config::WorkflowRole,
        error: &dyn std::fmt::Display,
    ) -> String {
        format!(
            "the {role} transformation did not complete under {}; the fixture workspace root is {}: {error}",
            root.display(),
            if root.is_dir() {
                "present"
            } else {
                "missing, so it was removed while the derivation was running"
            }
        )
    }

    #[cfg(test)]
    mod tests {
        use super::{
            derivation_failure, derive_workflows_under, workspace_directory_name, Workspace,
        };
        use crate::config::WorkflowRole;

        #[test]
        fn init_tests_use_the_shared_workspace_fixture() {
            let init_source = include_str!("../init.rs");
            assert!(
                init_source.contains("use crate::executor::fixture::Workspace;"),
                "init tests must import the shared workspace fixture"
            );
            assert!(
                !init_source.contains("struct TestDirectory"),
                "init tests must not transcribe a workspace fixture"
            );
            assert!(
                !init_source.contains("std::env::temp_dir()"),
                "init tests must not hand-roll a temporary fixture root"
            );
        }

        #[test]
        fn a_workspace_removes_its_root_when_dropped() {
            let root = {
                let workspace = Workspace::new("cleanup");
                let root = workspace.root().to_path_buf();
                assert!(root.is_dir(), "the workspace owns a created root");
                root
            };
            assert!(
                !root.exists(),
                "the workspace root must not survive its owner"
            );
        }

        /// The defect the CLI handoff fixtures were exposed to: one label,
        /// reached twice through a shared helper, inside one clock tick.
        #[test]
        fn one_label_within_one_clock_tick_still_names_distinct_workspaces() {
            let first = workspace_directory_name("shared-label", 1);
            let second = workspace_directory_name("shared-label", 1);
            assert_ne!(
                first, second,
                "a workspace name may not take its uniqueness from the label or the clock"
            );
            assert!(
                first.contains("shared-label"),
                "the label still says what the workspace is for: {first}"
            );
        }

        /// Smoke check with no witness, kept for the isolation assertion only.
        ///
        /// Distinctness over real `Workspace::new` calls holds without the
        /// sequence number nearly every time, so no mutation kills this alone.
        /// The uniqueness guard above and the collision refusal below are what
        /// carry the property; this one carries none of it.
        #[test]
        fn two_workspaces_under_one_label_own_separate_roots() {
            let first = Workspace::new("shared-label");
            let second = Workspace::new("shared-label");
            assert_ne!(first.root(), second.root());
            first.write("marker.txt", "first");
            assert!(!second.root().join("marker.txt").exists());
            assert!(first.root().is_dir() && second.root().is_dir());
        }

        #[test]
        #[should_panic(expected = "create workspace")]
        fn a_workspace_refuses_a_root_another_workspace_already_holds() {
            let held = Workspace::new("already-held");
            let _second = Workspace::at(held.root().to_path_buf());
        }

        #[test]
        fn a_derivation_failure_says_whether_the_workspace_survived() {
            let workspace = Workspace::new("derivation-diagnostic");
            let present = derivation_failure(workspace.root(), WorkflowRole::Release, &"io error");
            assert!(present.contains("root is present"), "{present}");
            assert!(
                present.contains(&workspace.root().display().to_string()),
                "the diagnostic names the root it examined: {present}"
            );
            assert!(
                present.contains("io error"),
                "the diagnostic carries the failure it explains: {present}"
            );

            let removed = workspace.root().join("gone");
            let missing = derivation_failure(&removed, WorkflowRole::Release, &"io error");
            assert!(
                missing.contains("removed while the derivation was running"),
                "{missing}"
            );
        }

        /// Which role failed is half of what makes the diagnostic actionable:
        /// one role deriving and the other not is a different defect from
        /// neither deriving.
        #[test]
        fn a_derivation_failure_names_the_role_that_failed() {
            let workspace = Workspace::new("derivation-role");
            let release = derivation_failure(workspace.root(), WorkflowRole::Release, &"io error");
            let publish = derivation_failure(workspace.root(), WorkflowRole::Publish, &"io error");
            assert!(
                release.contains(&WorkflowRole::Release.to_string()),
                "{release}"
            );
            assert!(
                publish.contains(&WorkflowRole::Publish.to_string()),
                "{publish}"
            );
            assert_ne!(release, publish, "the two roles read alike: {release}");
        }

        /// The wiring the residual claim rests on: a derivation that cannot
        /// complete reports through the diagnostic rather than through a bare
        /// `expect`, at whichever step gives out first.
        #[test]
        #[should_panic(expected = "the fixture workspace root is")]
        fn a_derivation_that_cannot_complete_reports_through_the_diagnostic() {
            let workspace = Workspace::new("derivation-unwired");
            let _ = derive_workflows_under(workspace.root());
        }
    }
}
