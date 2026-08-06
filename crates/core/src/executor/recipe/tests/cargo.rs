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
            let error = select_publications(workspace.root(), &config("    cargo: {}\n"))
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
        let selected = select_publications(workspace.root(), &config("    cargo: {}\n"))
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
