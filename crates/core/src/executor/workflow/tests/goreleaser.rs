// ---
// relationships:
//   implements: github-release-executor
// ---

// GoReleaser-backed route derivation tests moved from `executor::workflow::tests`.

    /// A workspace publishing a GoReleaser subject beside a registry-only one.
    ///
    /// The upload job's barrier is unconditional while the job itself is not, and
    /// only a release holding both kinds of publication can tell those two apart.
    /// A Go-only release makes every publisher a handoff consumer and a
    /// registry-only release derives no upload job at all, so in either one a
    /// barrier wired to the handoff and a barrier wired to the job are the same
    /// graph.
    fn mixed_workspace(label: &str) -> Workspace {
        let workspace = go_workspace(label);
        workspace
            .write(
                ".intentional/config.yml",
                &GO_CONFIG.replace(
                    "release-units:\n  component:\n",
                    "release-units:\n  library:\n    path: library\n    packages:\n      package:\n        path: .\n        npm: {}\n    tags:\n      staged:\n        role: primary\n        template: 'library/staged@{version}'\n        require-phase: before-publication\n  component:\n",
                ),
            )
            .write(
                "library/package.json",
                r#"{"name":"@example-owner/example-library","version":"1.0.0"}"#,
            );
        workspace
    }

    /// Native GoReleaser configuration declaring every pipe the recipes promote.
    ///
    /// The `nfpms` entry declares two formats beyond the two the system-package
    /// adapters distribute, and each is there for its own reason. `apk` is a
    /// format the repository can ask for and the adapters do not distribute, so
    /// it separates a derivation that reads the declaration from one that names
    /// the pair it happens to know. `archlinux` is the format whose package does
    /// not carry the format's own name, so it separates a derivation that maps
    /// each format to the extension its package carries from one that assumes
    /// the two are spelled alike.
    const GORELEASER_CONFIG: &str = r#"version: 2
project_name: example-tool
builds:
  - main: ./cmd/example-tool
brews:
  - repository: { owner: example-org, name: homebrew-tap }
nfpms:
  - formats: [ rpm, deb, apk, archlinux ]
aur:
  - name: example-tool-bin
"#;

    fn go_workspace(label: &str) -> Workspace {
        let workspace = workspace_without_package(label);
        workspace
            .write(".intentional/config.yml", GO_CONFIG)
            // The module's last element is deliberately not the release-unit
            // id, so a derivation that fell back to the id is distinguishable
            // from one that read the module.
            .write("component/go.mod", "module example.test/example-module\n")
            .write(
                "component/cmd/example-tool/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write("component/.goreleaser.yaml", GORELEASER_CONFIG);
        workspace
    }

    // The sealed subject identity is what a publisher fragment is compared
    // against, so a Go release unit that named the release-unit id would agree
    // with the seal by making both sides wrong. GoReleaser names the formula,
    // the system packages, and the Arch package from the project name, so that
    // is the one identity every destination resolves.
    #[test]
    fn names_the_native_goreleaser_project_as_the_subject_identity() {
        let workspace = go_workspace("workflow-go-identity");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let build = "intentional_build_component_goreleaser";
        let identity = jobs[&Value::String(build.to_owned())]["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find_map(|step| step["with"]["identity"].as_str())
            .expect("the build job records a subject identity");
        assert_eq!(
            identity, "example-tool",
            "the subject identity is the native project name, not the release-unit id"
        );

        // Without an explicit project name the module path's last element is
        // what Go names the command, and it is still native evidence rather
        // than the release-unit id.
        workspace.write(
            "component/.goreleaser.yaml",
            &GORELEASER_CONFIG.replace("project_name: example-tool\n", ""),
        );
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let identity = jobs[&Value::String(build.to_owned())]["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find_map(|step| step["with"]["identity"].as_str())
            .expect("the build job records a subject identity");
        assert_eq!(identity, "example-module");
    }

    /// Steps of one derived job, as name-and-body pairs.
    fn job_steps(jobs: &serde_yaml::Mapping, id: &str) -> Vec<Value> {
        jobs[&Value::String(id.to_owned())]["steps"]
            .as_sequence()
            .expect("steps")
            .clone()
    }

    /// Every `run:` body one derived job executes, concatenated.
    fn job_run_bodies(jobs: &serde_yaml::Mapping, id: &str) -> String {
        job_steps(jobs, id)
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // GoReleaser's open-source distribution cannot publish a dist tree a
    // previous invocation built, so a recipe that reached its destination by
    // running the packager again would rebuild from source and produce a
    // subject whose digest cannot equal the one the release sealed. This is the
    // rule the whole build-once graph rests on for Go.
    #[test]
    fn promotes_what_the_go_build_produced_rather_than_running_the_packager_again() {
        let workspace = go_workspace("workflow-go-promote");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let builds = job_ids(&jobs, "intentional_build_");
        assert_eq!(
            builds,
            vec!["intentional_build_component_goreleaser".to_owned()],
            "two destinations of one packager derive one build job: {builds:?}"
        );
        assert!(
            job_run_bodies(&jobs, &builds[0]).contains("goreleaser release"),
            "the build job is where the packager runs"
        );

        let publishers = job_ids(&jobs, "intentional_publish_");
        assert_eq!(publishers.len(), 2, "{publishers:?}");
        for publisher in &publishers {
            let body = job_run_bodies(&jobs, publisher);
            assert!(
                !body.contains("goreleaser"),
                "{publisher} promotes what the build produced rather than rebuilding it: {body}"
            );
            let downloads = job_steps(&jobs, publisher)
                .iter()
                .filter_map(|step| step["with"]["name"].as_str())
                .filter(|name| name.starts_with("intentional_subject-"))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(
                downloads,
                vec!["intentional_subject-component_goreleaser".to_owned()],
                "{publisher} consumes the one built subject"
            );
        }
    }



    // A build job that cannot run its packager produces nothing, and this job
    // is the sole producer of every Go deliverable the publisher jobs promote.
    // A stock runner does not carry GoReleaser.
    #[test]
    fn installs_the_packager_the_go_build_job_runs() {
        let workspace = go_workspace("workflow-go-toolchain");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let build = "intentional_build_component_goreleaser";
        let steps = job_steps(&jobs, build);
        let installer = steps
            .iter()
            .position(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.starts_with("goreleaser/goreleaser-action@"))
            })
            .expect("the build job installs the packager");
        assert_eq!(
            steps[installer]["uses"].as_str(),
            Some("goreleaser/goreleaser-action@f06c13b6b1a9625abc9e6e439d9c05a8f2190e94"),
            "the installer retains an independent literal witness of its complete commit identity"
        );
        assert_eq!(
            steps[installer]["with"]["install-only"].as_bool(),
            Some(true),
            "the installer installs the command; the build step runs it"
        );
        assert_eq!(
            steps[installer]["with"]["version"].as_str(),
            Some("2.17.1"),
            "the packager whose output layout these recipes read is pinned too"
        );
        let build_step = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("goreleaser"))
            })
            .expect("the build job runs the packager");
        assert!(
            installer < build_step,
            "the packager is installed before it is run"
        );

        // A packager a runner already carries derives no installer, so this is
        // a per-packager statement rather than a step every build job gained.
        let workspace = two_destination_workspace("workflow-buildx-toolchain");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        assert!(
            !job_steps(&jobs, "intentional_build_component_buildx")
                .iter()
                .any(|step| step["uses"]
                    .as_str()
                    .is_some_and(|uses| uses.contains("goreleaser"))),
            "an unrelated packager derives no GoReleaser installer"
        );
    }


    // Verification runs in the publisher unless a recipe isolates retrieval in
    // a narrower job. Its handoff follows it: a draft-dependent verifier without
    // one is refused by `verify publication`, and a verifier that reads no draft
    // asset supplying one is refused by the same command. Neither refusal is
    // reachable from here, so the derivation has to get both sides right.
    #[test]
    fn names_the_draft_handoff_only_for_a_publisher_that_reads_one() {
        for (label, workspace, draft_dependent) in [
            ("go", go_workspace("workflow-handoff-go"), true),
            ("npm", npm_workspace("workflow-handoff-npm"), false),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let jobs = publish_jobs(workspace.root());
            let publishers = job_ids(&jobs, "intentional_publish_");
            assert!(!publishers.is_empty(), "{label} derives a publisher job");
            for publisher in publishers {
                let retrieval = publisher.replace("intentional_publish_", "intentional_retrieve_");
                let handoff = [publisher.as_str(), retrieval.as_str()]
                    .into_iter()
                    .filter(|id| jobs.contains_key(Value::String((*id).to_owned())))
                    .flat_map(|id| job_steps(&jobs, id))
                    .find_map(|step| {
                        step["with"]["draft-handoff"]
                            .as_str()
                            .map(std::borrow::ToOwned::to_owned)
                    })
                    .unwrap_or_else(|| panic!("{publisher} verifies its publication"));
                assert_eq!(
                    !handoff.is_empty(),
                    draft_dependent,
                    "{publisher} names the handoff its consumer path reads: {handoff:?}"
                );
                if draft_dependent {
                    assert!(
                        handoff.contains(&publisher.replace("intentional_publish_", ""))
                            && handoff.ends_with(crate::publication::draft::DRAFT_HANDOFF_FILE),
                        "{publisher} reads the handoff for its own publication: {handoff:?}"
                    );
                }
            }
        }
    }
