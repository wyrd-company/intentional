// ---
// relationships:
//   implements: github-release-executor
// ---

// Arch User Repository route derivation tests moved from
// `executor::workflow::tests`.

    // The Arch User Repository is a Git host of its own rather than a GitHub
    // repository, so an installation token reaches nothing there.
    #[test]
    fn reaches_the_arch_user_repository_through_its_own_authority() {
        let workspace = go_workspace("workflow-go-aur");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let aur = "intentional_publish_component_package_aur_primary";

        assert!(
            !job_steps(&jobs, aur)
                .iter()
                .any(|step| step["id"].as_str() == Some("intentional_destination_token")),
            "no installation token is minted for a destination GitHub does not host"
        );
        let body = job_run_bodies(&jobs, aur);
        assert!(body.contains("ssh://aur@aur.archlinux.org/"), "{body}");
        assert!(body.contains("${INTENTIONAL_AUR_KEY}"), "{body}");
        let environment = job_steps(&jobs, aur)
            .into_iter()
            .find_map(|step| {
                step["env"]["INTENTIONAL_AUR_KEY"]
                    .as_str()
                    .map(str::to_owned)
            })
            .expect("the promotion reads the key from the environment");
        assert_eq!(
            environment, "${{ secrets.INTENTIONAL_AUR_KEY }}",
            "the recipe names a conventional secret and never carries its value"
        );
    }
