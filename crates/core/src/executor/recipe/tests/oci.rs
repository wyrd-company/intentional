// ---
// relationships:
//   implements: github-release-executor
// ---

// Open Container Initiative route tests moved from `executor::recipe::tests`.

    #[test]
    fn dockerfile_variant_is_detector_named_and_selects_a_real_publication() {
        let workspace = Workspace::new("dockerfile-variant-path");
        workspace.write("component/Dockerfile.alpine", "FROM scratch\n");
        let config = config("    oci:\n      ghcr: {}\n");

        let selected = select_publications(workspace.root(), &config)
            .expect("the detector variant selects its catalog route");
        assert_eq!(selected[0].capability, Capability::RunnableImage);
        assert_eq!(selected[0].packager, Packager::Buildx);
        assert!(
            publication_probe_paths(workspace.root(), &config)
                .expect("probe paths")
                .contains(Path::new("component/Dockerfile.alpine")),
            "the reader names the same variant the detector selected"
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
