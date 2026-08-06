// ---
// relationships:
//   implements: github-release-executor
// ---

// Homebrew route derivation tests moved from `executor::workflow::tests`.

    // A tap is a different repository from the one being released, and the App
    // is installed on it narrowly. A token minted without naming that repository
    // would carry every permission the App holds everywhere it is installed.
    #[test]
    fn mints_the_tap_token_for_the_configured_repository_alone() {
        let workspace = go_workspace("workflow-go-token");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let homebrew = "intentional_publish_component_package_homebrew_primary";

        let token = job_steps(&jobs, homebrew)
            .into_iter()
            .find(|step| step["id"].as_str() == Some("intentional_destination_token"))
            .expect("the homebrew job mints a destination token");
        assert_eq!(token["with"]["owner"].as_str(), Some("example-org"));
        assert_eq!(token["with"]["repository"].as_str(), None);
        assert_eq!(token["with"]["repositories"].as_str(), Some("homebrew-tap"));

        // The promotion reads the token through the environment and writes to
        // the configured tap, and both values reach the shell as variables
        // rather than as text spliced into the body.
        let body = job_run_bodies(&jobs, homebrew);
        assert!(
            body.contains("${INTENTIONAL_DESTINATION}") && body.contains("${GITHUB_TOKEN}"),
            "{body}"
        );
        assert!(
            !body.contains("example-org/homebrew-tap"),
            "the destination reaches the shell as a variable: {body}"
        );
    }
