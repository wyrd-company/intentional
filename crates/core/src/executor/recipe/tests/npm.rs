// ---
// relationships:
//   implements: github-release-executor
// ---

// npm route tests moved from `executor::recipe::tests`.

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
