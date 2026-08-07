// ---
// relationships:
//   implements: github-release-executor
// ---

// Cargo route tests moved from `executor::recipe::tests`.

    #[test]
    fn honors_cargo_publish_restrictions() {
        let workspace = Workspace::new("cargo-publish");
        for restriction in ["publish = false", "publish = []"] {
            workspace.write(
                "component/Cargo.toml",
                &format!("[package]\nname = \"component\"\n{restriction}\n"),
            );
            let derived =
                derive_component(workspace.root(), &config("")).expect("capabilities derive");
            assert!(
                capability_set(&derived).is_empty(),
                "{restriction} withholds the rust-crate capability"
            );
            let error = select_publications(workspace.root(), &config("    cargo: { registry: {} }\n"))
                .expect_err("unpublishable crate rejected");
            assert!(
                error
                    .to_string()
                    .contains("matching manifest declines publication"),
                "{error}"
            );
        }

        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"component\"\npublish = true\n",
        );
        let selected = select_publications(workspace.root(), &config("    cargo: { registry: {} }\n"))
            .expect("publishable crate selects");
        assert_eq!(selected[0].destination.as_deref(), Some("crates.io"));
    }


    #[test]
    fn resolves_the_concrete_cargo_primary_destination() {
        let workspace = Workspace::new("cargo");
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"component\"\nversion = \"1.0.0\"\n",
        );
        let selected = select_publications(workspace.root(), &config("    cargo: { registry: {} }\n"))
            .expect("cargo publication selects");
        assert_eq!(selected[0].destination.as_deref(), Some("crates.io"));

        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"component\"\npublish = [\"example-registry\"]\n",
        );
        let selected = select_publications(workspace.root(), &config("    cargo: { registry: {} }\n"))
            .expect("configured registry selects");
        assert_eq!(selected[0].destination.as_deref(), Some("example-registry"));
    }

    #[test]
    fn refuses_a_manifest_that_names_more_than_one_cargo_registry() {
        let workspace = Workspace::new("cargo-multi-registry");
        workspace.write(
            ".cargo/config.toml",
            "[registries.one]\nindex = \"sparse+https://one.example/\"\n[registries.two]\nindex = \"sparse+https://two.example/\"\n",
        );
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"component\"\nversion = \"1.0.0\"\npublish = [\"one\", \"two\"]\n",
        );
        let error = select_publications(workspace.root(), &config("    cargo: { registry: {} }\n"))
            .expect_err("multiple registries rejected");
        assert!(
            error
                .to_string()
                .contains("exactly one primary destination"),
            "{error}"
        );
    }

    #[test]
    fn open_catalog_selects_each_non_go_system_package_route() {
        let routes = catalog()
            .iter()
            .filter(|recipe| {
                recipe.capability != Capability::GoApplication
                    && matches!(
                        recipe.publisher,
                        PublisherKind::Rpm | PublisherKind::Apt | PublisherKind::Aur
                    )
            })
            .map(|recipe| (recipe.capability, recipe.packager, recipe.publisher))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            routes,
            BTreeSet::from([
                (
                    Capability::RustCrate,
                    Packager::CargoArchive,
                    PublisherKind::Rpm,
                ),
                (
                    Capability::RustCrate,
                    Packager::CargoArchive,
                    PublisherKind::Apt,
                ),
                (
                    Capability::RustCrate,
                    Packager::CargoArchive,
                    PublisherKind::Aur,
                ),
            ])
        );

        let workspace = Workspace::new("cargo-system-routes");
        workspace
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"sample-utility\"\nversion = \"1.0.0\"\n",
            )
            .write("component/src/main.rs", "fn main() {}\n");
        let selected = select_publications(
            workspace.root(),
            &config(
                "    rpm:\n      delivery-action: .github/actions/deliver-rpm\n      base-url: https://packages.invalid/rpm\n      public-signing-key-url: https://packages.invalid/key.asc\n      observation-deadline: 47\n      channel: stable\n      with: {}\n    apt:\n      delivery-action: .github/actions/deliver-apt\n      base-url: https://packages.invalid/apt\n      public-signing-key-url: https://packages.invalid/key.asc\n      observation-deadline: 47\n      suite: current\n      component: main\n      with: {}\n    aur: {}\n",
            ),
        )
        .expect("Cargo package selects the maintained system routes");
        assert_eq!(selected.len(), routes.len());
        for route in &selected {
            assert_eq!(route.capability, Capability::RustCrate);
            assert_eq!(route.packager, Packager::CargoArchive);
        }
        let destinations = selected
            .iter()
            .map(|route| (route.publisher, route.destination.as_deref()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            destinations,
            BTreeMap::from([
                (PublisherKind::Rpm, Some("https://packages.invalid/rpm")),
                (PublisherKind::Apt, Some("https://packages.invalid/apt")),
                (PublisherKind::Aur, Some("sample-utility-bin")),
            ])
        );

        let explicit_workspace = Workspace::new("cargo-system-explicit-binary");
        explicit_workspace
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"sample-container\"\nversion = \"1.0.0\"\nautobins = false\n\n[[bin]]\nname = \"sample-runner\"\npath = \"src/runner.rs\"\n",
            )
            .write("component/src/runner.rs", "fn main() {}\n");
        let explicit = select_publications(
            explicit_workspace.root(),
            &config("    aur: {}\n"),
        )
        .expect("one explicit Cargo binary selects AUR");
        assert_eq!(explicit[0].destination.as_deref(), Some("sample-runner-bin"));
    }

    #[test]
    fn refuses_an_aur_route_without_one_cargo_binary_identity() {
        let workspace = Workspace::new("cargo-aur-identity-refusal");
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"sample-library\"\nversion = \"1.0.0\"\n",
        );
        let error = select_publications(workspace.root(), &config("    aur: {}\n"))
            .expect_err("a library cannot guess an Arch package repository");
        assert!(
            error
                .to_string()
                .contains("cannot derive one native executable identity"),
            "{error}"
        );
    }

    #[test]
    fn discovers_one_cargo_auto_binary_under_src_bin() {
        for (path, expected) in [
            ("component/src/bin/sample-tool.rs", "sample-tool-bin"),
            ("component/src/bin/sample-runner/main.rs", "sample-runner-bin"),
        ] {
            let workspace = Workspace::new("cargo-auto-binary");
            workspace
                .write(
                    "component/Cargo.toml",
                    "[package]\nname = \"sample-package\"\nversion = \"1.0.0\"\n",
                )
                .write(path, "fn main() {}\n");

            let selected = select_publications(workspace.root(), &config("    aur: {}\n"))
                .expect("one Cargo auto-binary selects the maintained native route");
            assert_eq!(selected[0].destination.as_deref(), Some(expected));
        }
    }

    #[test]
    fn names_the_auto_binaries_that_make_native_identity_ambiguous() {
        let workspace = Workspace::new("cargo-auto-binary-ambiguity");
        workspace
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"sample-package\"\nversion = \"1.0.0\"\n",
            )
            .write("component/src/bin/first-tool.rs", "fn main() {}\n")
            .write("component/src/bin/second-tool/main.rs", "fn main() {}\n");

        let error = select_publications(workspace.root(), &config("    aur: {}\n"))
            .expect_err("several auto-binaries are refused");
        assert!(
            error.to_string().contains(
                "found Cargo binary targets [first-tool, second-tool], but the maintained native packager requires exactly one [[bin]].name or Cargo auto-binary"
            ),
            "{error}"
        );
    }

    #[test]
    fn refuses_an_explicit_cargo_binary_beside_an_unclaimed_auto_binary() {
        let workspace = Workspace::new("cargo-mixed-binary-ambiguity");
        workspace
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"sample-package\"\nversion = \"1.0.0\"\n\n[[bin]]\nname = \"first-tool\"\npath = \"src/first.rs\"\n",
            )
            .write("component/src/first.rs", "fn main() {}\n")
            .write("component/src/bin/second-tool.rs", "fn main() {}\n");

        let error = select_publications(workspace.root(), &config("    aur: {}\n"))
            .expect_err("an explicit and an unclaimed auto-binary are several targets");
        assert!(
            error.to_string().contains(
                "found Cargo binary targets [first-tool, second-tool], but the maintained native packager requires exactly one"
            ),
            "{error}"
        );
    }

    #[test]
    fn an_explicit_cargo_path_claims_its_auto_discovered_file() {
        for declaration in [
            "name = \"renamed-tool\"\npath = \"src/bin/sample-tool.rs\"",
            "name = \"sample-tool\"",
        ] {
            let workspace = Workspace::new("cargo-explicit-binary-claim");
            workspace
                .write(
                    "component/Cargo.toml",
                    &format!(
                        "[package]\nname = \"sample-package\"\nversion = \"1.0.0\"\n\n[[bin]]\n{declaration}\n"
                    ),
                )
                .write("component/src/bin/sample-tool.rs", "fn main() {}\n");

            let selected = select_publications(workspace.root(), &config("    aur: {}\n"))
                .expect("the explicit declaration and claimed auto path are one target");
            let expected = if declaration.contains("renamed-tool") {
                "renamed-tool-bin"
            } else {
                "sample-tool-bin"
            };
            assert_eq!(selected[0].destination.as_deref(), Some(expected));
        }
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
