// ---
// relationships:
//   implements: github-release-executor
// ---

// GoReleaser-backed route tests moved from `executor::recipe::tests`.

    // A draft-dependent recipe either retrieves the draft asset directly or
    // delivers that asset to a public system-package repository before its
    // clean-client retrieval. Every draft-dependent publisher must take one of
    // those paths, and no other publisher may take either.
    #[test]
    fn agrees_with_the_draft_handoff_about_which_destinations_read_a_draft_asset() {
        for recipe in catalog() {
            let consumes_draft = recipe.retrieval == CleanClientMode::AuthenticatedDraft
                || matches!(recipe.publisher, PublisherKind::Rpm | PublisherKind::Apt);
            assert_eq!(
                consumes_draft,
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
            let command = files
                .iter()
                .find(|(relative, _)| relative.ends_with(".go"))
                .expect("layout carries a Go source")
                .0;
            let package_path = Path::new(command)
                .parent()
                .expect("source has parent")
                .strip_prefix("component")
                .expect("source belongs to component")
                .to_string_lossy();
            let package_path = if package_path.is_empty() {
                "."
            } else {
                &package_path
            };
            let layout_config = config_at(package_path, "");
            let derived =
                derive_component(workspace.root(), &layout_config).expect("capabilities derive");
            assert!(
                capability_set(&derived).contains(&Capability::GoApplication),
                "{layout} derives the go-application capability"
            );

            let selected = select_publications(
                workspace.root(),
                &config_at(
                    package_path,
                    "    homebrew: { repository: example-org/homebrew-tap }\n",
                ),
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
    fn withholds_the_capability_from_a_command_in_a_nested_go_module() {
        let workspace = Workspace::new("go-nested-module");
        workspace
            .write("component/go.mod", "module example.test/component\n")
            .write("component/nested/go.mod", "module example.test/nested\n")
            .write(
                "component/nested/main.go",
                "package main\n\nfunc main() {}\n",
            );
        let derived = derive_component(workspace.root(), &config("")).expect("capabilities derive");
        assert!(
            !capability_set(&derived).contains(&Capability::GoApplication),
            "a child module command does not make the parent module publishable"
        );
    }

    #[test]
    fn derives_a_declared_package_from_its_own_nested_go_module() {
        let workspace = Workspace::new("declared-nested-go-module");
        workspace
            .write("component/tool/go.mod", "module example.test/tool\n")
            .write("component/tool/main.go", "package main\n\nfunc main() {}\n");
        let selected = select_publications(
            workspace.root(),
            &config_at(
                "tool",
                "    homebrew: { repository: example-org/homebrew-tap }\n",
            ),
        )
        .expect("the package owns the module at its declared path");
        assert_eq!(selected[0].capability, Capability::GoApplication);
    }

    #[test]
    fn withholds_the_capability_from_commands_go_excludes_from_recursive_packages() {
        let workspace = Workspace::new("go-excluded-packages");
        workspace
            .write("component/go.mod", "module example.test/component\n")
            .write(
                "component/vendor/example.test/other/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write(
                "component/testdata/sample/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write(
                "component/_fixtures/sample/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write(
                "component/.fixtures/sample/main.go",
                "package main\n\nfunc main() {}\n",
            );
        let derived = derive_component(workspace.root(), &config("")).expect("capabilities derive");
        assert!(
            !capability_set(&derived).contains(&Capability::GoApplication),
            "commands outside Go's recursive package set do not make the module publishable"
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
        let error = select_publications(workspace.root(), &config("    npm: { npmjs: {} }\n"))
            .expect_err("an unsupported combination is refused");
        assert!(
            error
                .to_string()
                .contains("derived capabilities are go-application"),
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
                "component/package/aur/primary cannot publish templated aur[0].name in component/.goreleaser.yaml"
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
