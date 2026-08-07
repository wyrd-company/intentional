// ---
// relationships:
//   implements: github-release-executor
// ---

// npm and Cargo registry route tests moved from `executor::workflow::tests`.

    /// A workspace publishing one npm package to both of its destinations.
    ///
    /// The npm adapter is the only one whose two destinations differ in what
    /// they will accept — one serves anonymous clients and implements trusted
    /// publishing, the other serves neither — so it is the workspace every
    /// per-destination property is asserted against.
    fn npm_workspace(label: &str) -> Workspace {
        let workspace = workspace(label);
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    cargo: { registry: {} }\n",
                    "    npm: { npmjs: {}, github: {} }\n",
                ),
            )
            .write(
                "component/package.json",
                r#"{"name":"@example-owner/example-component","version":"1.0.0"}"#,
            );
        std::fs::remove_file(workspace.root().join("component/Cargo.toml")).expect("remove");
        workspace
    }

    /// Publication and retrieval steps for one managed destination.
    fn publisher_steps(root: &Path, target: &str) -> Vec<Value> {
        let managed = managed_steps(root, WorkflowRole::Publish);
        let publisher = managed
            .iter()
            .find(|(id, _)| id.ends_with(target) && id.starts_with("intentional_publish_"))
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| panic!("the {target} publisher is derived"));
        let jobs = publish_jobs(root);
        let (_, verifier_steps, _) = publication_verification(&jobs, &publisher);
        let observer_steps = verifier_steps
            .iter()
            .filter_map(portable_observer_step)
            .collect::<Vec<_>>();
        managed
            .into_iter()
            .filter(|(id, _)| id == &publisher)
            .flat_map(|(_, steps)| steps)
            .chain(observer_steps)
            .chain(verifier_steps)
            .collect()
    }

    /// One managed job's steps, addressed by its complete job id.
    fn managed_job_steps(root: &Path, expected: &str) -> Vec<Value> {
        managed_steps(root, WorkflowRole::Publish)
            .into_iter()
            .find(|(id, _)| id == expected)
            .unwrap_or_else(|| panic!("managed job {expected} is derived"))
            .1
    }

    /// One step's `env:` mapping, as plain strings.
    fn step_environment(step: &Value) -> BTreeMap<String, String> {
        step.get("env")
            .and_then(Value::as_mapping)
            .map(|env| {
                env.iter()
                    .filter_map(|(key, value)| {
                        Some((key.as_str()?.to_owned(), value.as_str()?.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The npm client performs the publish while the job carries publication
    /// authority, so its complete version is part of the emitted recipe rather
    /// than a lower bound resolved again on every run.
    #[test]
    fn pins_the_trusted_publishing_npm_client_to_one_exact_version() {
        let workspace = npm_workspace("workflow-npm-client-pin");
        converge(workspace.root(), WorkflowRole::Publish);
        let step = publisher_steps(workspace.root(), "primary")
            .into_iter()
            .find(|step| {
                step["name"].as_str() == Some("Prepare the npm client for trusted publishing")
            })
            .expect("the primary npm recipe prepares its trusted-publishing client");
        let environment = step_environment(&step);
        assert_eq!(
            environment.get("INTENTIONAL_NPM_VERSION").map(String::as_str),
            Some("11.5.1"),
            "the publication client resolves no future npm release"
        );
        assert!(
            step["run"]
                .as_str()
                .is_some_and(|body| body.contains("npm install --global \"npm@${INTENTIONAL_NPM_VERSION}\"")),
            "the installer consumes the exact-version environment binding"
        );
    }

    // A recipe's readback writes the observation and the portable command reads
    // it, and the two agree only by path. Before the recipes existed nothing
    // wrote one at all: `verify publication` was handed a path that never came
    // into being, waited out its whole consistency deadline reading a file that
    // was not there, and failed as "pending" -- a diagnostic naming the
    // destination rather than the missing producer. Naming the file in two
    // places is exactly the shape the artifact binding above exists to stop, so
    // it is asserted for the observation too.
    #[test]
    fn binds_every_observation_a_publisher_verifies_to_the_step_that_writes_it() {
        let scripts = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/action/observe-publication");
        let dispatcher = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/action/observe-publication.sh"),
        )
        .expect("observer dispatcher");
        for publisher in PublisherKind::ALL {
            let (arm, adapter) = match publisher {
                PublisherKind::Npm => ("npm", "npm"),
                PublisherKind::Cargo => ("cargo", "cargo"),
                PublisherKind::Homebrew | PublisherKind::Aur => ("homebrew|aur", "repository"),
                PublisherKind::Rpm | PublisherKind::Apt => ("rpm|apt", "system-package"),
                PublisherKind::Oci => ("oci", "oci"),
            };
            assert!(
                dispatcher.contains(&format!(
                    "{arm}) source \"$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/{adapter}.sh\""
                )),
                "the {publisher} input selects {adapter}.sh"
            );
            let adapter_body =
                std::fs::read_to_string(scripts.join(format!("{adapter}.sh")))
                    .expect("observer adapter");
            assert!(
                adapter_body.contains("observe_present"),
                "the {publisher} adapter writes a present observation"
            );
            let workspace = match publisher {
                PublisherKind::Npm => npm_workspace("workflow-observation-npm"),
                PublisherKind::Cargo => workspace("workflow-observation-cargo"),
                PublisherKind::Homebrew => go_workspace("workflow-observation-homebrew"),
                PublisherKind::Rpm => system_package_workspace("workflow-observation-rpm"),
                PublisherKind::Apt => system_package_workspace("workflow-observation-apt"),
                PublisherKind::Aur => go_workspace("workflow-observation-aur"),
                PublisherKind::Oci => two_destination_workspace("workflow-observation-oci"),
            };
            converge(workspace.root(), WorkflowRole::Publish);
            let document: Value = serde_yaml::from_str(&workflow(
                workspace.root(),
                WorkflowRole::Publish,
            ))
            .expect("workflow parses");
            let publications = document["jobs"]
                .as_mapping()
                .expect("jobs")
                .iter()
                .filter_map(|(job, body)| {
                    Some((
                        job.as_str()?.to_owned(),
                        body["steps"].as_sequence()?.clone(),
                    ))
                })
                .filter_map(|(job, steps)| {
                    steps
                        .iter()
                        .any(|step| {
                            intentional_action(step).is_some_and(|(name, _)| {
                                name == "verify-publication"
                                    && step["with"]["publisher"].as_str()
                                        == Some(publisher.as_str())
                            })
                        })
                        .then_some((job, steps))
                })
                .collect::<Vec<_>>();
            let all_steps = document["jobs"]
                .as_mapping()
                .expect("jobs")
                .values()
                .filter_map(|body| body["steps"].as_sequence())
                .flatten()
                .collect::<Vec<_>>();
            assert!(
                !publications.is_empty(),
                "the exhaustive fixture emits a {publisher} publisher"
            );
            for (job, steps) in publications {
                let verified = steps
                    .iter()
                    .find_map(|step| {
                        let (name, _) = intentional_action(step)?;
                        (name == "verify-publication").then(|| {
                            step["with"]["observation"]
                                .as_str()
                                .expect("path")
                                .to_owned()
                        })
                    })
                    .expect("the publisher verifies its publication");
                let writers = all_steps
                    .iter()
                    .filter(|step| {
                        let candidate =
                            portable_observer_step(step).unwrap_or_else(|| (**step).clone());
                        candidate["env"]["INPUT_OBSERVATION"].as_str()
                            == Some(verified.as_str())
                            && candidate["env"]["INPUT_PUBLISHER"].as_str()
                                == Some(publisher.as_str())
                            && candidate["run"].as_str().is_some_and(|body| {
                                body.contains("observe-publication.sh")
                                    || body.contains("observe_present")
                            })
                    })
                    .count();
                assert_eq!(
                    writers, 1,
                    "exactly one recipe step writes the observation {verified} that {job} verifies"
                );
            }
        }
    }

    // Every observation a recipe writes is written by shell. A misspelled
    // member, a state carrying detail it may not carry, or a schema identity
    // edited in one copy and not another is a defect nothing in a Rust test
    // would see and that a release runner would surface three jobs later, as a
    // verification failure naming the destination rather than the recipe. The
    // helpers are therefore executed and what they wrote is loaded by the same
    // loader the command uses.
    //
    // All three documents are driven, and the present one matters most: it
    // carries the subject, packager, destination and retrieval -- every
    // affirmative claim the fragment is built from. An earlier version of this
    // test ran only the two states that carry no detail, and two mutations in
    // the present path survived the whole suite green.
    #[test]
    fn writes_every_observation_state_in_a_form_the_loader_accepts() {
        for (workspace, publisher) in [
            (workspace("workflow-observed-states-cargo"), "cargo"),
            (npm_workspace("workflow-observed-states-npm"), "npm"),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let readback = publisher_steps(workspace.root(), PRIMARY_TARGET)
                .into_iter()
                .find(|step| step_environment(step).contains_key("INPUT_OBSERVATION"))
                .expect("the recipe writes an observation");
            let environment = step_environment(&readback);
            let body = std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts/action/observe-publication/common.sh"),
            )
            .expect("portable observer common script");

            let temporary = workspace.root().join("runner");
            std::fs::create_dir_all(&temporary).expect("runner directory");
            let resolve = |value: &str| {
                if value.contains(".outputs.version }}") {
                    "1.0.0".to_owned()
                } else if value.contains(".outputs.digest }}") {
                    "sha256:1111111111111111111111111111111111111111111111111111111111111111"
                        .to_owned()
                } else {
                    value.replace("${{ runner.temp }}", &temporary.display().to_string())
                }
            };
            // Everything the adapter script computes before it writes a present
            // observation reaches the helper as a shell variable, so the
            // document can be driven without reaching a registry. These stand
            // in for exactly those values.
            let computed = [
                ("INTENTIONAL_TARGET", PRIMARY_TARGET),
                ("INTENTIONAL_PACKAGER_ID", publisher),
                ("INTENTIONAL_VERSION", "1.0.0"),
                (
                    "INTENTIONAL_SUBJECT_DIGEST",
                    "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                ),
                ("INTENTIONAL_PACKAGER_VERSION", "1.2.3"),
                ("INTENTIONAL_DESTINATION_DIGEST", "a-destination-checksum"),
                ("INTENTIONAL_RETRIEVAL_VERSION", "1.2.3"),
                ("INTENTIONAL_RETRIEVED_DIGEST", "a-destination-checksum"),
            ];

            for (state, call) in [
                (
                    ObservationState::Pending,
                    "observe_state pending",
                ),
                (
                    ObservationState::Conflict,
                    "observe_state conflict \"another release holds this version\"",
                ),
                (ObservationState::Present, "observe_present"),
            ] {
                let mut command = std::process::Command::new("bash");
                command.arg("-c").arg(format!("{body}\n{call}"));
                for (key, value) in &environment {
                    command.env(key, resolve(value));
                }
                for (key, value) in computed {
                    command.env(key, value);
                }
                let output = command.output().expect("the recipe script runs");
                assert!(
                    output.status.success(),
                    "{publisher} writes a {state} observation: {}",
                    String::from_utf8_lossy(&output.stderr)
                );

                let path = resolve(&environment["INPUT_OBSERVATION"]);
                let observed =
                    crate::publication::observation::PublicationObservation::load(Path::new(&path))
                        .unwrap_or_else(|error| {
                            panic!("{publisher} writes a loadable {state} observation: {error}")
                        });
                assert_eq!(observed.state, state);
                assert_eq!(
                    observed.identity(),
                    format!("component/package/{publisher}/{PRIMARY_TARGET}"),
                    "the observation names the publication its job publishes"
                );
                if state != ObservationState::Present {
                    continue;
                }
                let retrieval = observed.retrieval.expect("a present observation retrieves");
                assert_eq!(
                    retrieval.mode,
                    CleanClientMode::Public,
                    "the present document records the mode its recipe fixes"
                );
                // The retrieval client is routed separately from the packager
                // because for an OCI destination the two are different
                // programs. For a language registry they are the same program,
                // and that used to be true by construction: one value was
                // printed twice and could not disagree. Splitting them made it
                // a caller argument, so the identity that was structural is
                // asserted here instead of assumed.
                let packager = observed
                    .packager
                    .as_ref()
                    .expect("a present observation names its packager");
                assert_eq!(
                    packager.id, publisher,
                    "the present document names the packager whose recipe wrote it"
                );
                assert_eq!(
                    retrieval.client, publisher,
                    "a language registry is retrieved by the packager itself, so the routed client cannot name a program that never ran"
                );
                let subject = observed
                    .subject
                    .expect("a present observation has a subject");
                assert_eq!(subject.version, "1.0.0");
                assert_eq!(
                    observed
                        .destination
                        .expect("a present observation reads back")
                        .digest,
                    "a-destination-checksum"
                );
            }
        }
    }

    // Everything the observation says about the release comes from the build
    // job, which derived it from the verified global release tag and from the
    // bytes it emitted. A recipe that read its own package manifest instead
    // would publish whatever that manifest said and record it as agreement,
    // which is the one disagreement the sealed subject exists to catch.
    #[test]
    fn reads_every_published_subject_fact_from_the_job_that_built_it() {
        let workspace = npm_workspace("workflow-subject-facts");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        let build = "intentional_build_component_npm";
        let outputs = jobs[&Value::String(build.to_owned())]["outputs"]
            .as_mapping()
            .expect("the build job projects what it recorded")
            .iter()
            .filter_map(|(key, value)| Some((key.as_str()?.to_owned(), value.as_str()?.to_owned())))
            .collect::<BTreeMap<_, _>>();
        let recorder = managed_steps(workspace.root(), WorkflowRole::Publish)
            .into_iter()
            .find(|(id, _)| id == build)
            .expect("the build job is derived")
            .1
            .iter()
            .find_map(|step| {
                let (name, _) = intentional_action(step)?;
                (name == "record-built-subject")
                    .then(|| step["id"].as_str().expect("addressable").to_owned())
            })
            .expect("the build job records its subject through the Action");
        for key in ["version", "digest"] {
            assert_eq!(
                outputs.get(key).map(String::as_str),
                Some(format!("${{{{ steps.{recorder}.outputs.{key} }}}}").as_str()),
                "the build job projects the {key} the recording step derived"
            );
            assert!(
                action_outputs("record-built-subject").contains(key),
                "the record-built-subject Action declares {key}"
            );
        }

        for target in ["primary", "github"] {
            for step in publisher_steps(workspace.root(), target) {
                let environment = step_environment(&step);
                for (variable, key) in [
                    ("INTENTIONAL_VERSION", "version"),
                    ("INTENTIONAL_SUBJECT_DIGEST", "digest"),
                ] {
                    let Some(value) = environment.get(variable) else {
                        continue;
                    };
                    assert_eq!(
                        value,
                        &format!("${{{{ needs.{build}.outputs.{key} }}}}"),
                        "the {target} publisher reads the subject {key} from {build}"
                    );
                }
            }
        }
    }

    // A `${{ }}` expansion inside a `run:` body is textual substitution into
    // shell source before the shell ever runs, so a value carrying a quote or a
    // `$(...)` becomes executable text. These bodies hold registry credentials
    // and run in a job that later reaches the publication protocol, and the
    // same rule is already gated for the composite Actions this repository
    // publishes; the derived workflows were the surface it did not cover.
    #[test]
    fn routes_every_value_a_managed_script_reads_through_its_environment() {
        for workspace in [
            workspace("workflow-expansion-cargo"),
            npm_workspace("workflow-expansion-npm"),
            two_destination_workspace("workflow-expansion-oci"),
        ] {
            for role in WorkflowRole::ALL {
                converge(workspace.root(), role);
                for (id, steps) in managed_steps(workspace.root(), role) {
                    for step in &steps {
                        let body = step.get("run").and_then(Value::as_str).unwrap_or_default();
                        assert!(
                            !body.contains("${{"),
                            "{id} expands a workflow expression inside a run body: {body}"
                        );
                    }
                }
            }
        }
    }

    // A published version is immutable, so a rerun after a partial failure has
    // exactly one safe move: read the destination, and submit only where it can
    // establish that it did not accept the original operation. A script that
    // published first would turn every rerun into a terminal conflict at a
    // destination that already held the right bytes. Ordering inside the script
    // is where that lives, so it is asserted by relative position.
    #[test]
    fn reads_each_destination_before_submitting_anything_to_it() {
        for (workspace, submission) in [
            (workspace("workflow-rerun-cargo"), "cargo publish"),
            (npm_workspace("workflow-rerun-npm"), "npm publish"),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let publishers = managed_steps(workspace.root(), WorkflowRole::Publish)
                .into_iter()
                .filter(|(id, _)| id.starts_with("intentional_publish_"))
                .collect::<Vec<_>>();
            assert!(!publishers.is_empty(), "the workspace derives publishers");
            for (target, steps) in publishers {
                let step = steps
                    .into_iter()
                    .find(|step| {
                        step.get("run")
                            .and_then(Value::as_str)
                            .is_some_and(|body| body.contains(submission))
                    })
                    .unwrap_or_else(|| panic!("{target} submits its subject with {submission}"));
                let body = step["run"].as_str().expect("a script");
                let read = body
                    .find("the readback decides whether it is this release")
                    .expect("the script short-circuits on a version the destination already holds");
                let submit = body
                    .find(submission)
                    .expect("the script submits the promoted subject");
                assert!(
                    read < submit,
                    "the {target} publisher reads its destination before submitting to it"
                );
            }
        }
    }

    /// The one command a stub cargo has to actually perform.
    ///
    /// The probe creates its scratch crate and then works inside it, so a stub
    /// that no-ops `cargo new` leaves the script with nowhere to go and the
    /// failure looks like a classification rather than a missing directory.
    const CARGO_NEW: &str = "case \"$1\" in new) mkdir -p \"${@: -1}\"; exit 0 ;; esac\n";

    /// A directory holding one stub client that answers a scripted way.
    ///
    /// The recipes decide whether to reach a long-lived credential from what a
    /// registry client tells them, and the three answers that matter differ
    /// only in an exit status and a line of output. Asserting the decision
    /// therefore means running the script against a client that gives each
    /// answer, which is what this builds.
    fn stub_client(directory: &Path, client: &str, script: &str) -> PathBuf {
        let path = directory.join(client);
        std::fs::create_dir_all(directory).expect("stub directory");
        // Every stub writes to standard error on every call, including the
        // calls that succeed. Real clients do: npm emits warnings, notices and
        // its update notice there routinely. A silent stub is what let a helper
        // that merged the two streams pass -- the merged value was only ever
        // exercised against a client that had nothing to say.
        // The stub records beside itself rather than through a variable the
        // harness sets. A probe that is given an allowlisted environment does
        // not carry the harness's variables into the client, and a stub that
        // needed one would go silent exactly when the allowlist started
        // working -- which is a stub reporting on the harness rather than on
        // the derivation.
        std::fs::write(
            &path,
            format!(
                "#!/usr/bin/env bash\n@ARGUMENTS@\n@ENVIRONMENT@\necho '{client} warn Unknown env config \"registry-scope\"' >&2\n{script}\nexit 0\n"
            )
            .replace(
                "@ARGUMENTS@",
                &format!(
                    "printf '%s\\n' \"$*\" >> \"{}\"",
                    directory.join("calls.log").display()
                ),
            )
            .replace(
                "@ENVIRONMENT@",
                &format!("env >> \"{}\"", directory.join("env.log").display()),
            ),
        )
        .expect("stub written");
        let mut permissions = std::fs::metadata(&path)
            .expect("stub metadata")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&path, permissions).expect("stub is executable");
        directory.to_path_buf()
    }

    /// Everything one stub client was called with, one invocation per line.
    fn stub_calls(stubs: &Path) -> String {
        std::fs::read_to_string(stubs.join("calls.log")).unwrap_or_default()
    }

    /// Every environment one stub client was invoked in.
    fn stub_environment(stubs: &Path) -> String {
        std::fs::read_to_string(stubs.join("env.log")).unwrap_or_default()
    }

    /// Run one derived step's script with a stub client ahead of it on PATH.
    fn run_step(
        step: &Value,
        stubs: &Path,
        temporary: &Path,
        extra: &[(&str, &str)],
    ) -> (bool, String) {
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(step["run"].as_str().expect("a script"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    test_tool_path(&std::env::var("PATH").unwrap_or_default())
                ),
            )
            .env("HOME", temporary)
            .env("RUNNER_TEMP", temporary)
            .env("GITHUB_WORKSPACE", temporary)
            .env("GITHUB_ENV", temporary.join("github.env"));
        for (key, value) in step_environment(step) {
            command.env(
                key,
                value.replace("${{ runner.temp }}", &temporary.display().to_string()),
            );
        }
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = command.output().expect("the derived script runs");
        (output.status.success(), stub_calls(stubs))
    }

    /// Execute a language-packager readback for a destination that remains
    /// unresolved through its deadline, and load the observation it emits.
    fn unresolved_readback_observation(
        workspace: &Workspace,
        client: &str,
        stub: &str,
        subject_extension: &str,
    ) -> crate::publication::observation::PublicationObservation {
        converge(workspace.root(), WorkflowRole::Publish);
        let readback = publisher_steps(workspace.root(), PRIMARY_TARGET)
            .into_iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Read "))
            })
            .expect("the publisher has a readback step");
        let environment = step_environment(&readback);
        let temporary = workspace.root().join("unresolved-readback");
        let subject = environment["INPUT_SUBJECT"]
            .replace("${{ runner.temp }}", &temporary.display().to_string());
        std::fs::create_dir_all(&subject).expect("subject directory");
        std::fs::write(
            Path::new(&subject).join(format!("subject.{subject_extension}")),
            "sealed subject bytes",
        )
        .expect("sealed subject");
        let stubs = stub_client(&temporary.join("stubs"), client, stub);
        let (succeeded, calls) = run_step(
            &readback,
            &stubs,
            &temporary,
            &[("INPUT_DEADLINE", "0")],
        );
        assert!(
            succeeded,
            "an unresolved {client} destination is reported through an observation; calls: {calls}"
        );
        let observation = environment["INPUT_OBSERVATION"]
            .replace("${{ runner.temp }}", &temporary.display().to_string());
        crate::publication::observation::PublicationObservation::load(Path::new(&observation))
            .unwrap_or_else(|error| panic!("the {client} readback wrote an observation: {error}"))
    }

    fn assert_pending_without_components(
        observation: &crate::publication::observation::PublicationObservation,
        packager: &str,
    ) {
        assert_eq!(
            observation.state,
            ObservationState::Pending,
            "the accepted {packager} publication has not become observable"
        );
        assert!(
            observation.build_provenance.is_empty(),
            "a pending {packager} observation carries no build-provenance block"
        );
        assert!(
            observation.attached_metadata.is_empty(),
            "a pending {packager} observation carries no attached-metadata block"
        );
        assert!(
            observation.destination_aliases.is_empty(),
            "a pending {packager} observation carries no destination-aliases block"
        );
    }

    /// Witness: npmjs never resolves @example-owner/example-component@1.0.0
    /// before the readback deadline.
    #[test]
    fn reports_an_npm_destination_that_never_resolves_as_pending() {
        let workspace = npm_workspace("workflow-npm-readback-unresolved");
        let observation = unresolved_readback_observation(&workspace, "npm", "exit 1", "tgz");
        assert_pending_without_components(&observation, "npm");
    }

    /// Witness: crates.io never resolves example-component@1.0.0 before the
    /// readback deadline.
    #[test]
    fn reports_a_cargo_destination_that_never_resolves_as_pending() {
        let workspace = workspace("workflow-cargo-readback-unresolved");
        let observation = unresolved_readback_observation(
            &workspace,
            "cargo",
            &format!("{CARGO_NEW}exit 1"),
            "crate",
        );
        assert_pending_without_components(&observation, "Cargo");
    }


    // A destination the catalog names has one anonymity. A Cargo primary does
    // not: it resolves to whatever registry `package.publish` names, ordinarily
    // a private one, and the recipe authenticates it with a configured token
    // and then retrieves it with that token in the environment. Recording the
    // catalog's public default there asserts a consumer path nobody outside the
    // credential holder could take, which is the untruth the authenticated mode
    // was introduced to stop -- and the destination that most needed it was the
    // one that walked past it.
    #[test]
    fn records_an_alternate_cargo_registry_as_a_destination_without_anonymous_read() {
        let workspace = workspace("workflow-alternate-cargo-registry");
        workspace.write(
            ".cargo/config.toml",
            "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
        );
        workspace.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
        );
        converge(workspace.root(), WorkflowRole::Publish);

        let selected = crate::executor::recipe::select_publications(
            workspace.root(),
            &Config::load(workspace.root()).expect("configuration loads"),
        )
        .expect("publications select");
        assert_eq!(
            selected[0].retrieval,
            CleanClientMode::AuthenticatedRegistry,
            "an alternate registry admits no anonymous consumer path"
        );

        let written = publisher_steps(workspace.root(), PRIMARY_TARGET)
            .iter()
            .filter_map(|step| {
                step_environment(step)
                    .get("INPUT_RETRIEVAL_MODE")
                    .cloned()
            })
            .collect::<Vec<_>>();
        assert_eq!(written, vec!["authenticated-registry".to_owned()]);

        // The name never reaches a command as spliced text, whether or not it
        // would have been safe to splice.
        for step in publisher_steps(workspace.root(), PRIMARY_TARGET) {
            let body = step.get("run").and_then(Value::as_str).unwrap_or_default();
            assert!(
                !body.contains("--registry example-registry"),
                "the configured registry name reaches cargo as a quoted variable: {body}"
            );
        }
    }

    // The mode field states what the retrieval did, so the retrieval has to be
    // what the field says. The bootstrap path writes an auth token into the
    // job's own npm configuration and it persists for the rest of the job, so a
    // retrieval reading that file sends a credential while recording a public
    // consumer path. A fresh cache is not a fresh identity. Isolation is
    // invisible in every structural property of the graph -- the step is in the
    // right job, in the right order, writing the right document -- so the
    // configuration the retrieval runs under is asserted directly, together
    // with what that configuration is made to hold.
    #[test]
    fn retrieves_under_the_identity_the_recorded_mode_names() {
        let workspace = npm_workspace("workflow-clean-client-identity");
        converge(workspace.root(), WorkflowRole::Publish);
        let body = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/action/observe-publication/npm.sh"),
        )
        .expect("npm observer script");
        for (target, credentialed) in [(PRIMARY_TARGET, false), ("github", true)] {
            let readback = publisher_steps(workspace.root(), target)
                .into_iter()
                .find(|step| step_environment(step).contains_key("INPUT_OBSERVATION"))
                .expect("the recipe reads its destination back");
            let environment = step_environment(&readback);
            assert_eq!(
                environment["INPUT_RETRIEVAL_MODE"] == "authenticated-registry",
                credentialed,
                "the {target} destination records the identity it retrieves under"
            );
            // Everything between preparing the scratch directory and the
            // retrieval is how the retrieval's identity is decided.
            let (_, prepared) = body
                .split_once("mkdir -p \"$INTENTIONAL_WORK/clean\"")
                .expect("the step prepares a scratch directory for its retrieval");
            let (prepared, invocation) = prepared
                .split_once("npm pack")
                .expect("the recipe retrieves through the client's own path");

            assert!(
                prepared.contains("> \"$INTENTIONAL_WORK/clean/npmrc\""),
                "the {target} step writes the configuration its retrieval reads"
            );
            assert_eq!(
                environment
                    .get("INPUT_REGISTRY_TOKEN")
                    .is_some_and(|token| !token.is_empty()),
                credentialed,
                "the {target} observer receives a credential only where the recorded mode says one was used"
            );
            assert!(
                prepared.contains("if [[ \"$INTENTIONAL_RETRIEVAL_MODE\" == public ]]")
                    && prepared.contains("_authToken"),
                "the portable observer writes the credential only when the recorded mode requires one"
            );
            assert!(
                invocation
                    .lines()
                    .next()
                    .into_iter()
                    .chain(prepared.rsplit('\n').take(3))
                    .any(|line| line
                        .contains("npm_config_userconfig=\"$INTENTIONAL_WORK/clean/npmrc\"")),
                "the {target} retrieval runs under that configuration rather than the job's"
            );
        }
    }

    /// Witness: `@example-owner/example-component` publishes to GitHub Package
    /// Registry under write authority, then a distinct downstream job retrieves
    /// it under read authority using the build job's projected subject facts.
    #[test]
    fn isolates_github_package_retrieval_in_a_read_scoped_job() {
        let workspace = npm_workspace("workflow-github-package-reader");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let publisher = "intentional_publish_component_package_npm_github";
        let retrieval = "intentional_retrieve_component_package_npm_github";
        let build = "intentional_build_component_npm";
        let publisher_job = jobs
            .get(Value::String(publisher.to_owned()))
            .expect("the write-scoped publisher job is derived");
        let retrieval_job = jobs
            .get(Value::String(retrieval.to_owned()))
            .expect("the read-scoped retrieval job is derived");

        assert_eq!(
            publisher_job["permissions"]["packages"].as_str(),
            Some("write")
        );
        assert_eq!(
            retrieval_job["permissions"]["packages"].as_str(),
            Some("read")
        );
        let retrieval_needs = retrieval_job["needs"]
            .as_sequence()
            .expect("retrieval names its dependencies");
        for dependency in [publisher, build] {
            assert!(
                retrieval_needs.contains(&Value::String(dependency.to_owned())),
                "retrieval reads only after {dependency} completes"
            );
        }
        let after_needs = transitive_needs(&jobs, "intentional_tag_after_publication");
        for completed in [publisher, retrieval] {
            assert!(
                after_needs.contains(completed),
                "after-publication tag follows {completed}"
            );
        }
        let assembly_needs = transitive_needs(&jobs, "intentional_assemble_evidence");
        for completed in [publisher, retrieval] {
            assert!(
                assembly_needs.contains(completed),
                "evidence assembly follows {completed}"
            );
        }

        let publishing = managed_job_steps(workspace.root(), publisher);
        let retrieving = managed_job_steps(workspace.root(), retrieval);
        assert!(
            publishing
                .iter()
                .all(|step| !step_environment(step).contains_key("INPUT_OBSERVATION")),
            "the write-scoped job carries no consumer retrieval"
        );
        let readback = retrieving
            .iter()
            .find(|step| step_environment(step).contains_key("INPUT_OBSERVATION"))
            .expect("the read-scoped job performs consumer retrieval");
        let environment = step_environment(readback);
        assert_eq!(
            environment
                .get("INPUT_REGISTRY_TOKEN")
                .map(String::as_str),
            Some("${{ secrets.GITHUB_TOKEN }}")
        );
        for (variable, output) in [
            ("INPUT_SUBJECT_VERSION", "version"),
            ("INPUT_SUBJECT_DIGEST", "digest"),
        ] {
            assert_eq!(
                environment.get(variable).map(String::as_str),
                Some(format!("${{{{ needs.{build}.outputs.{output} }}}}").as_str()),
                "retrieval reads the subject {output} the build job projected"
            );
        }
    }

    // Authenticated observation uses the narrowest authority the destination
    // offers. GitHub Package Registry supplies a read-only job token, so its
    // inline observer runs in a separate packages:read job. An alternate Cargo
    // registry supplies no maintained read-only mint, so its inline observer
    // stays beside publication and the write credential is never copied into a
    // second job or handed to the first-party Action.
    #[test]
    fn keeps_authenticated_observers_at_the_narrowest_available_boundary() {
        let npm = npm_workspace("workflow-authenticated-observer-npm");
        converge(npm.root(), WorkflowRole::Publish);
        let npm_jobs = publish_jobs(npm.root());
        let npm_publisher = "intentional_publish_component_package_npm_github";
        let (npm_verifier, npm_steps, npm_action) =
            publication_verification(&npm_jobs, npm_publisher);
        assert_eq!(
            npm_jobs[&Value::String(npm_verifier.clone())]["permissions"]["packages"].as_str(),
            Some("read"),
            "GitHub Package Registry observation receives a read-only job token"
        );
        assert!(
            npm_steps.iter().any(|step| {
                step_environment(step).get("INPUT_REGISTRY_TOKEN").map(String::as_str)
                    == Some("${{ secrets.GITHUB_TOKEN }}")
            }),
            "the read-only token reaches only repository-visible observer shell"
        );
        assert_eq!(npm_action["with"]["observe"].as_str(), Some("false"));
        assert!(npm_action["with"]["registry-token"].is_null());

        let cargo = workspace("workflow-authenticated-observer-cargo");
        cargo.write(
            ".cargo/config.toml",
            "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
        );
        cargo.write(
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
        );
        converge(cargo.root(), WorkflowRole::Publish);
        let cargo_jobs = publish_jobs(cargo.root());
        let cargo_publisher = "intentional_publish_component_package_cargo_primary";
        assert!(
            !cargo_jobs.contains_key(Value::String(
                "intentional_retrieve_component_package_cargo_primary".to_owned()
            )),
            "no second job receives the alternate registry's write-capable token"
        );
        assert!(
            job_steps(&cargo_jobs, cargo_publisher).iter().any(|step| {
                step_environment(step)
                    .values()
                    .any(|value| value == "${{ secrets.CARGO_REGISTRY_TOKEN }}")
                    && step_environment(step).contains_key("INPUT_OBSERVATION")
            }),
            "authenticated Cargo observation spends the publisher's existing credential inline"
        );
        let (cargo_verifier, _, cargo_action) =
            publication_verification(&cargo_jobs, cargo_publisher);
        assert_eq!(
            cargo_verifier,
            "intentional_verify_component_package_cargo_primary"
        );
        assert_eq!(cargo_action["with"]["observe"].as_str(), Some("false"));
        assert!(cargo_action["with"]["carried-token"].is_null());
    }

    /// Witness: the read-scoped job for `@example-owner/example-component`
    /// receives `read-job-token`. Stub npm records the scratch configuration
    /// consumed by the emitted `npm pack` invocation.
    #[test]
    fn github_package_reader_invokes_npm_with_its_read_scoped_job_token() {
        let workspace = npm_workspace("workflow-github-package-reader-invocation");
        converge(workspace.root(), WorkflowRole::Publish);
        let readback = managed_job_steps(
            workspace.root(),
            "intentional_retrieve_component_package_npm_github",
        )
        .into_iter()
        .find(|step| step_environment(step).contains_key("INPUT_OBSERVATION"))
        .expect("the retrieval job reads the package back");
        let environment = step_environment(&readback);
        let temporary = workspace.root().join("github-package-reader");
        let subject = environment["INPUT_SUBJECT"]
            .replace("${{ runner.temp }}", &temporary.display().to_string());
        std::fs::create_dir_all(&subject).expect("subject directory");
        let tarball = Path::new(&subject).join("subject.tgz");
        std::fs::write(&tarball, "sealed package bytes").expect("sealed package");
        let observed = temporary.join("observed-npmrc");
        let stub = format!(
            "case \"$1\" in\n  config) mkdir -p \"$HOME\"; printf '%s\\n' \"$3\" >> \"$HOME/.npmrc\"; exit 0 ;;\n  view) grep -q ':_authToken=read-job-token' \"$HOME/.npmrc\" || exit 1; printf 'sha512-'; openssl dgst -sha512 -binary '{}' | base64 -w0; printf '\\n'; exit 0 ;;\n  pack) grep -q ':_authToken=read-job-token' \"$npm_config_userconfig\" || exit 1; cp '{}' \"$(pwd)/example-component-1.0.0.tgz\"; cp \"$npm_config_userconfig\" '{}'; exit 0 ;;\n  --version) printf '11.5.1\\n'; exit 0 ;;\nesac",
            tarball.display(),
            tarball.display(),
            observed.display(),
        );
        let stubs = stub_client(&temporary.join("stubs"), "npm", &stub);
        let (succeeded, calls) = run_step(
            &readback,
            &stubs,
            &temporary,
            &[
                ("INPUT_REGISTRY_TOKEN", "read-job-token"),
                ("INPUT_SUBJECT_VERSION", "1.0.0"),
                ("INPUT_SUBJECT_DIGEST", "unused-build-digest"),
                ("INPUT_DEADLINE", "0"),
            ],
        );
        assert!(
            succeeded,
            "read-scoped retrieval succeeds against stub npm: {calls}"
        );
        assert!(
            observed.exists(),
            "the authenticated destination probe reaches clean-client retrieval: {calls}"
        );
        let npmrc = std::fs::read_to_string(observed).expect("stub npm consumed scratch npmrc");
        assert!(
            npmrc.contains(":_authToken=read-job-token"),
            "the emitted npm invocation consumes the read-scoped job token"
        );
    }

    // Cargo spells "the index does not carry this crate" and "I did not look,
    // because I was told not to go online" with the same words, so the probe
    // refuses to be offline rather than trying to tell the two apart. That
    // setting, and every other the probe runs under, now come from one place:
    // the environment is cleared and rebuilt from an allowlist, so what is
    // asserted is the contents of that list and that every client invocation
    // runs under it. A variable the list does not name cannot reach the client,
    // which is the property four rounds of key-by-key closure never had.
    #[test]
    fn resolves_under_an_allowlisted_environment_that_forces_the_network_on() {
        for (label, workspace, manifest, carries) in [
            (
                "the crates.io primary",
                workspace("workflow-cargo-online-primary"),
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
                None,
            ),
            (
                "a configured alternate registry",
                {
                    let workspace = workspace("workflow-cargo-online-alternate");
                    workspace.write(
                        ".cargo/config.toml",
                        "[registries.example-registry]\nindex = \"sparse+https://registry.example/index/\"\n",
                    );
                    workspace
                },
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\npublish = [\"example-registry\"]\n",
                Some("CARGO_REGISTRIES_EXAMPLE_REGISTRY_TOKEN"),
            ),
        ] {
            workspace.write("component/Cargo.toml", manifest);
            converge(workspace.root(), WorkflowRole::Publish);
            let bodies = publisher_steps(workspace.root(), PRIMARY_TARGET)
                .into_iter()
                .filter_map(|step| step.get("run").and_then(Value::as_str).map(str::to_owned))
                .filter(|body| body.contains("INTENTIONAL_resolve()"))
                .collect::<Vec<_>>();
            assert!(
                !bodies.is_empty(),
                "{label} resolves its destination through cargo"
            );

            // Both names cargo reads are derived from the configured registry,
            // and the ordinary spelling of a registry carries a hyphen -- which
            // is not a character a variable name may hold, so a derivation that
            // passed the name through would name something no shell can set and
            // no client reads, and the alternate-registry path would fail on a
            // runner with neither a credential nor an index.
            let declared = |key: &str| {
                publisher_steps(workspace.root(), PRIMARY_TARGET)
                    .iter()
                    .filter_map(|step| step_environment(step).get(key).cloned())
                    .collect::<BTreeSet<_>>()
            };
            assert_eq!(
                declared("INTENTIONAL_REGISTRY_INDEX_VARIABLE"),
                [carries
                    .map(|_| "CARGO_REGISTRIES_EXAMPLE_REGISTRY_INDEX".to_owned())
                    .unwrap_or_default()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
                "{label} names the index variable cargo reads for it"
            );
            // The credential is named where the recorded retrieval is the
            // authenticated one and nowhere else, and it is named in `env:`
            // rather than in the script, so a case transform on the way to a
            // shell body has nothing to transform.
            assert_eq!(
                declared("INTENTIONAL_CARRIED_TOKEN"),
                [carries.map(str::to_owned).unwrap_or_default()]
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                "{label} carries only the credential its recorded retrieval uses"
            );
            for body in bodies {
                let allowlist = body
                    .split_once("INTENTIONAL_resolve()")
                    .map(|(head, _)| head)
                    .expect("the allowlist is built before the resolve uses it");
                assert!(
                    allowlist.contains("CARGO_NET_OFFLINE=false"),
                    "{label} forces the probe online: {allowlist}"
                );
                assert!(
                    allowlist.contains("${INTENTIONAL_CARRIED_TOKEN}"),
                    "{label} reads the carried credential's name rather than spelling it: {allowlist}"
                );

                // Every client invocation runs under the list. One that did not
                // would inherit the job's environment, which is where a
                // repository's own workflow-level `env:` arrives.
                let (_, resolve) = body
                    .split_once("INTENTIONAL_resolve()")
                    .expect("the resolve is a shell function");
                for command in ["cargo new", "cargo add", "cargo fetch"] {
                    let invocation = resolve
                        .split_once(command)
                        .map(|(head, _)| head)
                        .unwrap_or_else(|| panic!("{label} runs {command}"));
                    let preamble = invocation
                        .rsplit("&&")
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    assert!(
                        preamble.contains("env -i \"${INTENTIONAL_ALLOWED[@]}\""),
                        "{label} runs {command} under the allowlist: {preamble:?}"
                    );
                    assert!(
                        preamble.contains("CARGO_HOME=\"$1/home\""),
                        "{label} gives {command} a scratch CARGO_HOME: {preamble:?}"
                    );
                }
            }
        }
    }

    // The existence probe answers two questions with one call: whether the
    // destination holds the release, and what it holds. The second answer
    // becomes the destination digest the readback compares against the promoted
    // integrity, so anything else the client said on the way becomes a
    // disagreement the recipe reports as the registry publishing bytes the
    // release never sent -- a conflict observation that fails the publication
    // and accuses the wrong party. npm writes warnings and notices to standard
    // error on successful calls, so the two streams have to stay apart. This
    // runs the derived helper against a client that talks on both.
    #[test]
    fn answers_only_with_what_the_registry_answered() {
        let workspace = npm_workspace("workflow-probe-streams");
        converge(workspace.root(), WorkflowRole::Publish);
        let body = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/action/observe-publication/npm.sh"),
        )
        .expect("npm observer script");
        let start = body
            .find("npm_holds() {")
            .expect("the recipe defines an existence probe");
        let end = body[start..]
            .find("\n}\n")
            .expect("the probe is a shell function");
        let helper = &body[start..start + end + "\n}\n".len()];

        let temporary = workspace.root().join("runner");
        std::fs::create_dir_all(&temporary).expect("runner directory");
        let integrity = "sha512-anintegritythepublishedreleaseactuallycarries";
        for (label, script, expected, status) in [
            (
                "a successful call that also warns",
                format!("case \"$1\" in view) echo '{integrity}' ;; esac"),
                integrity,
                0,
            ),
            (
                "a package the registry does not hold",
                "case \"$1\" in view) echo 'npm error code E404' >&2; exit 1 ;; esac".to_owned(),
                "",
                1,
            ),
            (
                "a call the registry did not answer",
                "case \"$1\" in view) echo 'npm error network request failed' >&2; exit 1 ;; esac"
                    .to_owned(),
                "",
                2,
            ),
        ] {
            let stubs = stub_client(&temporary.join(status.to_string()), "npm", &script);
            let answer = temporary.join("answer");
            let mut command = std::process::Command::new("bash");
            command
                .arg("-c")
                .arg(format!(
                    "ALLOWED=(\"PATH=$PATH\")\nSCOPE_ARGUMENTS=()\n{helper}\nnpm_holds example > \"{}\"; exit $?",
                    answer.display()
                ))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        stubs.display(),
                        test_tool_path(&std::env::var("PATH").unwrap_or_default())
                    ),
                )
                .env("INTENTIONAL_WORK", &temporary)
                .env("INTENTIONAL_REGISTRY", "https://registry.example");
            let output = command.output().expect("the probe runs");
            assert_eq!(
                output.status.code(),
                Some(status),
                "{label} classifies as {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(&answer).unwrap_or_default(),
                expected,
                "{label} returns only what the registry answered"
            );
        }
    }

    #[test]
    fn classifies_only_cargo_registry_absence_as_missing() {
        let body = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/action/observe-publication/cargo.sh"),
        )
        .expect("Cargo observer script");
        let start = body
            .find("resolve() {")
            .expect("the observer defines its existence probe");
        let end = body[start..]
            .find("\n}\n")
            .expect("the probe is a shell function");
        let helper = &body[start..start + end + "\n}\n".len()];
        let temporary = tempfile::tempdir().expect("temporary Cargo probe directory");

        for (label, add, expected) in [
            ("resolved release", "exit 0", 0),
            (
                "release absent from registry",
                "echo 'could not be found in registry' >&2; exit 1",
                1,
            ),
            (
                "registry did not answer",
                "echo 'network request failed' >&2; exit 1",
                2,
            ),
        ] {
            let stub = format!(
                "if [[ \"$1\" == new ]]; then mkdir -p \"${{@: -1}}\"; exit 0; fi\n{add}"
            );
            let stubs = stub_client(&temporary.path().join(expected.to_string()), "cargo", &stub);
            let output = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "ALLOWED=(\"PATH=$PATH\")\nREGISTRY_ARGUMENTS=()\n{helper}\nresolve \"{}\"",
                    temporary.path().join(format!("probe-{expected}")).display()
                ))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        stubs.display(),
                        test_tool_path(&std::env::var("PATH").unwrap_or_default())
                    ),
                )
                .env("INTENTIONAL_SUBJECT_IDENTITY", "sample-library")
                .env("INTENTIONAL_VERSION", "1.2.3")
                .output()
                .expect("the Cargo probe runs");
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{label} classifies as {expected}: {}; calls: {}",
                String::from_utf8_lossy(&output.stderr),
                std::fs::read_to_string(stubs.join("calls.log")).unwrap_or_default()
            );
        }
    }

    // A probe that did not succeed is not evidence of absence. `npm view` and
    // `cargo add` fail the same way on a missing package, a rate limit, a proxy
    // failure and a 5xx, and absence is the one condition that unlocks the
    // long-lived bootstrap token. Collapsing the two turns any transient
    // registry failure into a steady-state publication authenticated by a
    // long-lived credential instead of the configured trusted identity -- the
    // silent fallback the design forbids, arrived at without anything saying
    // so. The ordering assertion below cannot see this, because it asserts
    // where the token is read and not what the probe proved, so the script is
    // run against a client that gives each answer.
    #[test]
    fn refuses_a_bootstrap_token_when_the_probe_did_not_answer() {
        let inconclusive = "npm error network request to https://registry.example failed";
        for (workspace, client, absent, inconclusive, present) in [
            (
                workspace("workflow-probe-cargo"),
                "cargo",
                CARGO_NEW.to_owned()
                    + "case \"$1\" in add) echo 'error: the crate could not be found in registry index' >&2; exit 1 ;; esac",
                CARGO_NEW.to_owned()
                    + "case \"$1\" in add) echo 'error: failed to fetch; connection reset' >&2; exit 1 ;; esac",
                String::new(),
            ),
            (
                npm_workspace("workflow-probe-npm"),
                "npm",
                "case \"$1\" in view) echo 'npm error code E404' >&2; exit 1 ;; esac".to_owned(),
                format!("case \"$1\" in view) echo '{inconclusive}' >&2; exit 1 ;; esac"),
                "case \"$1\" in view) echo 'sha512-abc' ;; esac".to_owned(),
            ),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let authenticate = publisher_steps(workspace.root(), PRIMARY_TARGET)
                .into_iter()
                .find(|step| {
                    step_environment(step).contains_key("INTENTIONAL_BOOTSTRAP_TOKEN")
                })
                .expect("the primary publisher authenticates");
            let temporary = workspace.root().join("runner");
            std::fs::create_dir_all(&temporary).expect("runner directory");
            let token = [("INTENTIONAL_BOOTSTRAP_TOKEN", "a-long-lived-token")];

            let stubs = stub_client(&temporary.join("absent"), client, &absent);
            let (succeeded, log) = run_step(&authenticate, &stubs, &temporary, &token);
            assert!(
                succeeded,
                "a proven first publication reaches its bootstrap token"
            );
            assert!(
                log.contains("a-long-lived-token")
                    || std::fs::read_to_string(temporary.join("github.env"))
                        .unwrap_or_default()
                        .contains("a-long-lived-token"),
                "the bootstrap path presents the token: {log}"
            );

            let stubs = stub_client(&temporary.join("inconclusive"), client, &inconclusive);
            std::fs::write(temporary.join("github.env"), "").expect("reset");
            let (succeeded, log) = run_step(&authenticate, &stubs, &temporary, &token);
            assert!(
                !succeeded,
                "a probe that did not answer refuses to decide: {log}"
            );
            assert!(
                !log.contains("a-long-lived-token")
                    && !std::fs::read_to_string(temporary.join("github.env"))
                        .unwrap_or_default()
                        .contains("a-long-lived-token"),
                "an inconclusive probe never presents the bootstrap token: {log}"
            );

            if present.is_empty() {
                continue;
            }
            let stubs = stub_client(&temporary.join("present"), client, &present);
            std::fs::write(temporary.join("github.env"), "").expect("reset");
            let (succeeded, log) = run_step(&authenticate, &stubs, &temporary, &token);
            assert!(succeeded, "an existing package authenticates: {log}");
            assert!(
                !log.contains("a-long-lived-token"),
                "steady-state publication never presents the bootstrap token: {log}"
            );
        }
    }

    // The bootstrap token exists because a registry cannot bind a trusted
    // publisher to a package it does not hold yet. That is its whole warrant,
    // and it holds only while the package is absent: a recipe that read the
    // secret first and probed afterwards would have a long-lived credential in
    // hand on every steady-state publication, which is the fallback the design
    // forbids. Ordering inside one script is the only place that can be seen,
    // so it is asserted by relative position rather than by presence.
    #[test]
    fn reaches_a_bootstrap_token_only_after_proving_the_package_is_absent() {
        for (workspace, secret) in [
            (
                workspace("workflow-bootstrap-cargo"),
                "secrets.CARGO_REGISTRY_TOKEN",
            ),
            (npm_workspace("workflow-bootstrap-npm"), "secrets.NPM_TOKEN"),
        ] {
            converge(workspace.root(), WorkflowRole::Publish);
            let steps = publisher_steps(workspace.root(), "primary");
            let holders = steps
                .iter()
                .filter(|step| {
                    step_environment(step)
                        .values()
                        .any(|value| value.contains(secret))
                })
                .collect::<Vec<_>>();
            let [authenticate] = holders.as_slice() else {
                panic!("exactly one step of the primary publisher reads {secret}");
            };
            let body = authenticate["run"].as_str().expect("a script");
            let probe = body
                .find("INTENTIONAL_SUBJECT_IDENTITY")
                .expect("the script resolves the package before deciding");
            let read = body
                .find("${INTENTIONAL_BOOTSTRAP_TOKEN:-}")
                .expect("the script reads the bootstrap token defensively");
            assert!(
                probe < read,
                "the primary publisher probes for the package before reaching its bootstrap token"
            );
        }
    }

    #[test]
    fn npmjs_token_secret_reaches_only_the_npmjs_destination() {
        let workspace = npm_workspace("workflow-npmjs-token-isolation");
        let config = std::fs::read_to_string(workspace.root().join(".intentional/config.yml"))
            .expect("configuration reads")
            .replace(
                "npm: { npmjs: {}, github: {} }",
                "npm: { npmjs: { token-secret: EXAMPLE_BOOTSTRAP_TOKEN }, github: {} }",
            );
        workspace.write(".intentional/config.yml", &config);
        converge(workspace.root(), WorkflowRole::Publish);

        let primary = publisher_steps(workspace.root(), PRIMARY_TARGET);
        let holders = primary
            .iter()
            .filter(|step| {
                step_environment(step)
                    .values()
                    .any(|value| value == "${{ secrets.EXAMPLE_BOOTSTRAP_TOKEN }}")
            })
            .collect::<Vec<_>>();
        let [authenticate] = holders.as_slice() else {
            panic!("exactly one npmjs step reads its configured token-secret");
        };
        assert_eq!(
            step_environment(authenticate)
                .get("INTENTIONAL_BOOTSTRAP_TOKEN")
                .map(String::as_str),
            Some("${{ secrets.EXAMPLE_BOOTSTRAP_TOKEN }}")
        );

        let github = publisher_steps(workspace.root(), "github");
        assert!(
            github.iter().all(|step| step_environment(step)
                .values()
                .all(|value| !value.contains("EXAMPLE_BOOTSTRAP_TOKEN"))),
            "the npmjs token-secret never reaches the GitHub Packages peer"
        );
        let github_tokens = github
            .iter()
            .filter_map(|step| {
                step_environment(step)
                    .get("INTENTIONAL_GITHUB_PACKAGES_TOKEN")
                    .cloned()
            })
            .collect::<Vec<_>>();
        assert!(!github_tokens.is_empty(), "the GitHub peer authenticates");
        assert!(
            github_tokens
                .iter()
                .all(|value| value == "${{ secrets.GITHUB_TOKEN }}"),
            "the GitHub peer uses only its job-scoped token: {github_tokens:?}"
        );
    }

    // The recipe fixes what its destination admits, and the observation it
    // writes has to say the same thing or `verify publication` refuses it. Both
    // sides are derived here, so a destination whose recipe changed one and not
    // the other fails at derivation instead of on a release runner.
    #[test]
    fn writes_the_retrieval_mode_the_maintained_recipe_fixes() {
        let workspace = npm_workspace("workflow-retrieval-mode");
        converge(workspace.root(), WorkflowRole::Publish);
        for (target, mode) in [
            (PRIMARY_TARGET, CleanClientMode::Public),
            ("github", CleanClientMode::AuthenticatedRegistry),
        ] {
            let selected = crate::executor::recipe::select_publications(
                workspace.root(),
                &Config::load(workspace.root()).expect("configuration loads"),
            )
            .expect("publications select")
            .into_iter()
            .find(|publication| publication.target == target)
            .expect("the target is configured");
            assert_eq!(selected.retrieval, mode, "the catalog fixes {target}");

            let written = publisher_steps(workspace.root(), target)
                .iter()
                .filter_map(|step| {
                    step_environment(step)
                        .get("INPUT_RETRIEVAL_MODE")
                        .cloned()
                })
                .collect::<Vec<_>>();
            let expected = match mode {
                CleanClientMode::Public => "public",
                CleanClientMode::AuthenticatedRegistry => "authenticated-registry",
                CleanClientMode::AuthenticatedDraft => "authenticated-draft",
            };
            assert_eq!(
                written,
                vec![expected.to_owned()],
                "the {target} recipe records the retrieval its catalog entry fixes"
            );
        }
    }

    // The recipe waits for its own destination and the command that reads its
    // observation waits under the same bound. Two hand-written copies of that
    // policy drift silently: a recipe that gave up sooner would report pending
    // for a release that was about to appear, and one that outlasted the
    // command would keep a runner busy past the point anything could accept it.
    #[test]
    fn waits_under_the_bound_the_maintained_policy_states() {
        let workspace = workspace("workflow-consistency-policy");
        converge(workspace.root(), WorkflowRole::Publish);
        let policy =
            crate::publication::observation::ConsistencyPolicy::maintained(PublisherKind::Cargo);
        let readback = publisher_steps(workspace.root(), "primary")
            .into_iter()
            .find(|step| step_environment(step).contains_key("INPUT_DEADLINE"))
            .expect("the recipe reads its destination back under a bound");
        let environment = step_environment(&readback);
        for (variable, seconds) in [
            ("INPUT_INTERVAL", policy.interval.as_secs()),
            (
                "INPUT_MAXIMUM_INTERVAL",
                policy.maximum_interval.as_secs(),
            ),
            ("INPUT_DEADLINE", policy.deadline.as_secs()),
        ] {
            assert_eq!(
                environment.get(variable).map(String::as_str),
                Some(seconds.to_string().as_str()),
                "{variable} is the maintained policy's value"
            );
        }
        assert_eq!(
            environment.get("INPUT_BACKOFF").map(String::as_str),
            Some(policy.backoff.to_string().as_str())
        );
    }

    // A workflow identity is a credential every step of the job it is granted
    // in can reach. Granting it to a destination whose recipe never presents
    // one costs nothing visible and is therefore exactly the kind of scope that
    // accumulates, so the jobs that hold it are named.
    #[test]
    fn grants_a_workflow_identity_only_where_a_recipe_presents_one() {
        let workspace = npm_workspace("workflow-identity-scope");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for (target, granted) in [("primary", true), ("github", false)] {
            let id = job_ids(&jobs, "intentional_publish_")
                .into_iter()
                .find(|id| id.ends_with(target))
                .expect("the publisher job is derived");
            let permissions = jobs[&Value::String(id.clone())]["permissions"]
                .as_mapping()
                .expect("a publisher states its permissions");
            assert_eq!(
                permissions.contains_key(Value::String("id-token".to_owned())),
                granted,
                "{id} holds a workflow identity only if its recipe presents one"
            );
        }
    }
