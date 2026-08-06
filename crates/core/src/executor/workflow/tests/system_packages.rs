// ---
// relationships:
//   implements: github-release-executor
// ---

// RPM and APT route derivation tests moved from `executor::workflow::tests`.

    const SYSTEM_PACKAGE_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
workspace-tags:
  release: { template: '{version}' }
github:
  prefix: release-automation
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml, gates: [ artifact_check ] }
release-units:
  component:
    path: component
    packages:
      package:
        path: .
        rpm:
          delivery-action: .github/actions/deliver-rpm
          base-url: https://packages.invalid/rpm/
          public-signing-key-url: https://packages.invalid/rpm-key.asc
          observation-deadline: 47
          channel: stable
          with:
            credential: '${{ secrets.DELIVERY_TOKEN }}'
            variable: '${{ vars.DELIVERY_BUCKET }}'
            literal: unchanged
            unavailable-context: '${{ matrix.destination }}'
        apt:
          delivery-action: .github/actions/deliver-apt
          base-url: https://packages.invalid/apt
          public-signing-key-url: https://packages.invalid/apt-key.asc
          observation-deadline: 53
          suite: current
          component: section-a
          with:
            apt-credential: '${{ secrets.APT_DELIVERY_TOKEN }}'
            apt-variable: '${{ vars.APT_DELIVERY_BUCKET }}'
            apt-literal: apt-unchanged
            apt-unavailable-context: '${{ matrix.apt_destination }}'
    tags:
      staged: { role: primary, template: '{id}/staged@{version}', require-phase: before-publication }
      published: { role: projection, template: '{id}/published@{version}', require-phase: after-publication }
"#;

    fn delivery_action(inputs: &[&str], using: &str) -> String {
        let inputs = inputs
            .iter()
            .map(|name| format!("  {name}: {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let recording = inputs
            .lines()
            .filter_map(|line| line.trim().strip_suffix(": {}"))
            .map(|name| format!("        printf '%s=%s\\n' '{name}' '${{{{ inputs.{name} }}}}' >> \"${{RECORDING}}\""))
            .collect::<Vec<_>>()
            .join("\n");
        format!("name: delivery\ninputs:\n{inputs}\nruns:\n  using: {using}\n  steps:\n    - shell: bash\n      run: |\n{recording}\n")
    }

    fn system_package_workspace(label: &str) -> Workspace {
        let workspace = go_workspace(label);
        workspace.write(".intentional/config.yml", SYSTEM_PACKAGE_CONFIG);
        let common = [
            "release-automation-package-path",
            "release-automation-format",
            "release-automation-name",
            "release-automation-version",
            "release-automation-architecture",
            "release-automation-digest",
            "release-automation-future",
        ];
        let mut rpm = common.to_vec();
        rpm.extend([
            "release-automation-rpm-channel",
            "credential",
            "variable",
            "literal",
            "unavailable-context",
        ]);
        let mut apt = common.to_vec();
        apt.extend([
            "release-automation-apt-suite",
            "release-automation-apt-component",
            "apt-credential",
            "apt-variable",
            "apt-literal",
            "apt-unavailable-context",
        ]);
        workspace
            .write(
                ".github/actions/deliver-rpm/action.yml",
                &delivery_action(&rpm, "composite"),
            )
            .write(
                ".github/actions/deliver-apt/action.yaml",
                &delivery_action(&apt, "composite"),
            );
        workspace
    }

    fn blocked_diagnostics(workspace: &Workspace) -> Vec<String> {
        let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
            .expect("comparison runs");
        assert_eq!(comparison.status, ComparisonStatus::Blocked);
        comparison
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect()
    }

    #[test]
    fn derives_system_package_delivery_calls_under_the_non_default_prefix() {
        let workspace = system_package_workspace("system-package-prefix");
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("workflow parses");
        let jobs = document["jobs"].as_mapping().expect("jobs");
        for (job, coordinate, coordinate_value, action, format, deadline, base_url, key_url) in [
            (
                "release_automation_publish_component_package_rpm_primary",
                "release-automation-rpm-channel",
                "stable",
                "./.github/actions/deliver-rpm",
                "rpm",
                "47",
                "https://packages.invalid/rpm/",
                "https://packages.invalid/rpm-key.asc",
            ),
            (
                "release_automation_publish_component_package_apt_primary",
                "release-automation-apt-suite",
                "current",
                "./.github/actions/deliver-apt",
                "deb",
                "53",
                "https://packages.invalid/apt",
                "https://packages.invalid/apt-key.asc",
            ),
        ] {
            let step = jobs[job]["steps"]
                .as_sequence()
                .expect("steps")
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Deliver "))
                })
                .expect("delivery step");
            assert_eq!(
                step["with"]["release-automation-package-path"].as_str(),
                Some("${{ steps.intentional_establish.outputs.path }}")
            );
            assert_eq!(step["with"][coordinate].as_str(), Some(coordinate_value));
            assert_eq!(
                step["with"]["release-automation-format"].as_str(),
                Some(format)
            );
            assert_eq!(
                step["with"]["release-automation-name"].as_str(),
                Some("${{ steps.intentional_establish.outputs.name }}")
            );
            assert_eq!(
                step["with"]["release-automation-version"].as_str(),
                Some("${{ steps.intentional_establish.outputs.version }}")
            );
            assert_eq!(
                step["with"]["release-automation-architecture"].as_str(),
                Some("${{ steps.intentional_establish.outputs.architecture }}")
            );
            assert_eq!(
                step["with"]["release-automation-digest"].as_str(),
                Some("${{ steps.intentional_establish.outputs.digest }}")
            );
            assert!(step["with"].get("intentional-package-path").is_none());
            assert_eq!(step["uses"].as_str(), Some(action));
            let readback = jobs[job]["steps"]
                .as_sequence()
                .expect("steps")
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Read back "))
                })
                .expect("readback step");
            assert_eq!(
                readback["env"]["RELEASE_AUTOMATION_DESTINATION"].as_str(),
                Some(base_url),
                "the product-shaped base URL reaches the readback unchanged"
            );
            assert_eq!(
                readback["env"]["RELEASE_AUTOMATION_PUBLIC_KEY_URL"].as_str(),
                Some(key_url)
            );
            assert_eq!(
                readback["env"]["RELEASE_AUTOMATION_DEADLINE"].as_str(),
                Some(deadline)
            );
        }
        let apt_readback = jobs["release_automation_publish_component_package_apt_primary"]
            ["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Read back "))
            })
            .expect("readback step");
        assert_eq!(
            apt_readback["env"]["RELEASE_AUTOMATION_APT_COMPONENT"].as_str(),
            Some("section-a"),
            "the product-shaped APT component reaches its readback consumer"
        );
        let rpm = &jobs["release_automation_publish_component_package_rpm_primary"]["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Deliver "))
            })
            .expect("delivery step")["with"];
        assert_eq!(
            rpm["credential"].as_str(),
            Some("${{ secrets.DELIVERY_TOKEN }}")
        );
        assert_eq!(
            rpm["variable"].as_str(),
            Some("${{ vars.DELIVERY_BUCKET }}")
        );
        assert_eq!(rpm["literal"].as_str(), Some("unchanged"));
        assert_eq!(
            rpm["unavailable-context"].as_str(),
            Some("${{ matrix.destination }}")
        );
        let apt = &jobs["release_automation_publish_component_package_apt_primary"]["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Deliver "))
            })
            .expect("delivery step")["with"];
        assert_eq!(
            apt["apt-credential"].as_str(),
            Some("${{ secrets.APT_DELIVERY_TOKEN }}")
        );
        assert_eq!(
            apt["apt-variable"].as_str(),
            Some("${{ vars.APT_DELIVERY_BUCKET }}")
        );
        assert_eq!(apt["apt-literal"].as_str(), Some("apt-unchanged"));
        assert_eq!(
            apt["apt-unavailable-context"].as_str(),
            Some("${{ matrix.apt_destination }}")
        );
    }

    #[test]
    fn executes_each_derived_delivery_call_against_its_recording_action() {
        let workspace = system_package_workspace("system-package-delivery-recording");
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("workflow parses");
        let outputs = [
            ("path", "/runner/subject/example-tool.pkg"),
            ("name", "example-tool"),
            ("version", "1.2.3"),
            ("architecture", "arm64"),
            (
                "digest",
                "sha256:4df1176a73c8a18d44f8b4db0df4808205205a5b88c42d36d95321aeecccc213",
            ),
        ];
        for (job, expected) in [
            (
                "release_automation_publish_component_package_rpm_primary",
                vec![
                    ("release-automation-package-path", outputs[0].1),
                    ("release-automation-format", "rpm"),
                    ("release-automation-name", outputs[1].1),
                    ("release-automation-version", outputs[2].1),
                    ("release-automation-architecture", outputs[3].1),
                    ("release-automation-digest", outputs[4].1),
                    ("release-automation-rpm-channel", "stable"),
                ],
            ),
            (
                "release_automation_publish_component_package_apt_primary",
                vec![
                    ("release-automation-package-path", outputs[0].1),
                    ("release-automation-format", "deb"),
                    ("release-automation-name", outputs[1].1),
                    ("release-automation-version", outputs[2].1),
                    ("release-automation-architecture", outputs[3].1),
                    ("release-automation-digest", outputs[4].1),
                    ("release-automation-apt-suite", "current"),
                    ("release-automation-apt-component", "section-a"),
                ],
            ),
        ] {
            let step = document["jobs"][job]["steps"]
                .as_sequence()
                .expect("steps")
                .iter()
                .find(|step| {
                    step["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("Deliver "))
                })
                .expect("delivery step");
            let action = step["uses"]
                .as_str()
                .expect("uses")
                .trim_start_matches("./");
            let metadata: Value = serde_yaml::from_str(
                &std::fs::read_to_string(workspace.root().join(action).join("action.yml"))
                    .or_else(|_| {
                        std::fs::read_to_string(workspace.root().join(action).join("action.yaml"))
                    })
                    .expect("Action metadata"),
            )
            .expect("Action metadata parses");
            let mut script = metadata["runs"]["steps"][0]["run"]
                .as_str()
                .expect("recording script")
                .to_owned();
            for (name, value) in step["with"].as_mapping().expect("with") {
                let name = name.as_str().expect("input name");
                let mut value = value.as_str().expect("input value").to_owned();
                for (output, replacement) in outputs {
                    value = value.replace(
                        &format!("${{{{ steps.intentional_establish.outputs.{output} }}}}"),
                        replacement,
                    );
                }
                script = script.replace(&format!("${{{{ inputs.{name} }}}}"), &value);
            }
            let recording = workspace.root().join(format!("{job}.inputs"));
            let status = std::process::Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("RECORDING", &recording)
                .status()
                .expect("delivery Action runs");
            assert!(status.success());
            let recorded = std::fs::read_to_string(recording).expect("recorded inputs");
            for (name, value) in expected {
                assert!(
                    recorded
                        .lines()
                        .any(|line| line == format!("{name}={value}")),
                    "{job} did not receive {name}={value}:\n{recorded}"
                );
            }
        }
    }

    #[test]
    fn refuses_a_delivery_action_that_is_not_composite() {
        let workspace = system_package_workspace("system-package-kind");
        workspace.write(
            ".github/actions/deliver-rpm/action.yml",
            &delivery_action(&[], "node20"),
        );
        assert!(blocked_diagnostics(&workspace)
            .iter()
            .any(|message| message.contains("runs.using: composite")));
    }

    #[test]
    fn refuses_a_delivery_action_missing_a_reserved_input() {
        let workspace = system_package_workspace("system-package-reserved");
        workspace.write(
            ".github/actions/deliver-rpm/action.yml",
            &delivery_action(&["release-automation-format"], "composite"),
        );
        assert!(blocked_diagnostics(&workspace)
            .iter()
            .any(|message| message.contains("reserved input release-automation-package-path")));
    }

    #[test]
    fn refuses_a_configured_delivery_key_the_action_does_not_declare() {
        let workspace = system_package_workspace("system-package-configured-key");
        let metadata = std::fs::read_to_string(
            workspace
                .root()
                .join(".github/actions/deliver-rpm/action.yml"),
        )
        .expect("metadata");
        workspace.write(
            ".github/actions/deliver-rpm/action.yml",
            &metadata.replace("  credential: {}\n", ""),
        );
        assert!(blocked_diagnostics(&workspace)
            .iter()
            .any(|message| message.contains("configured input \"credential\"")));
    }

    #[test]
    fn refuses_an_uncovered_required_delivery_input() {
        let workspace = system_package_workspace("system-package-required");
        let path = workspace
            .root()
            .join(".github/actions/deliver-rpm/action.yml");
        let metadata = std::fs::read_to_string(&path)
            .expect("metadata")
            .replace("inputs:\n", "inputs:\n  uncovered: { required: true }\n");
        workspace.write(".github/actions/deliver-rpm/action.yml", &metadata);
        assert!(blocked_diagnostics(&workspace)
            .iter()
            .any(|message| message.contains("requires input \"uncovered\" without a default")));
    }

    #[test]
    fn refuses_any_configured_key_inside_the_derived_namespace() {
        let workspace = system_package_workspace("system-package-namespace");
        let config = SYSTEM_PACKAGE_CONFIG.replace(
            "          with:\n            credential:",
            "          with:\n            release-automation-future: plain\n            credential:",
        );
        workspace.write(".intentional/config.yml", &config);
        assert!(blocked_diagnostics(&workspace)
            .iter()
            .any(|message| message.contains("release-automation-future")
                && message.contains("reserved namespace")));
    }

    #[test]
    fn refuses_a_delivery_directory_with_both_metadata_filenames() {
        let workspace = system_package_workspace("system-package-two-metadata-files");
        let metadata = std::fs::read_to_string(
            workspace
                .root()
                .join(".github/actions/deliver-rpm/action.yml"),
        )
        .expect("metadata");
        workspace.write(".github/actions/deliver-rpm/action.yaml", &metadata);
        assert!(blocked_diagnostics(&workspace)
            .iter()
            .any(|message| message.contains("must contain exactly one")));
    }

    #[test]
    fn refuses_a_delivery_directory_without_metadata() {
        let workspace = system_package_workspace("system-package-missing-metadata");
        let config = SYSTEM_PACKAGE_CONFIG
            .replace(".github/actions/deliver-rpm", ".github/actions/missing-rpm");
        workspace.write(".intentional/config.yml", &config);
        assert!(blocked_diagnostics(&workspace)
            .iter()
            .any(|message| message.contains("missing-rpm")
                && message.contains("must contain exactly one")));
    }

    #[test]
    fn refuses_an_absolute_delivery_action_path() {
        let workspace = system_package_workspace("system-package-absolute-action");
        workspace.write(
            ".intentional/config.yml",
            &SYSTEM_PACKAGE_CONFIG.replace(
                ".github/actions/deliver-rpm",
                "/workspace/actions/deliver-rpm",
            ),
        );
        assert!(blocked_diagnostics(&workspace).iter().any(|message| {
            message.contains("/workspace/actions/deliver-rpm")
                && message.contains("workspace-relative directory")
        }));
    }

    #[test]
    fn refuses_a_delivery_action_path_that_escapes_the_workspace() {
        let workspace = system_package_workspace("system-package-parent-action");
        workspace.write(
            ".intentional/config.yml",
            &SYSTEM_PACKAGE_CONFIG.replace(
                ".github/actions/deliver-rpm",
                ".github/actions/../deliver-rpm",
            ),
        );
        assert!(blocked_diagnostics(&workspace).iter().any(|message| {
            message.contains(".github/actions/../deliver-rpm")
                && message.contains("workspace-relative directory")
        }));
    }

    #[cfg(windows)]
    #[test]
    fn refuses_a_windows_drive_relative_delivery_action_path() {
        let workspace = system_package_workspace("system-package-windows-prefix-action");
        workspace.write(
            ".intentional/config.yml",
            &SYSTEM_PACKAGE_CONFIG.replace(".github/actions/deliver-rpm", "C:actions/deliver-rpm"),
        );
        assert!(blocked_diagnostics(&workspace).iter().any(|message| {
            message.contains("C:actions/deliver-rpm")
                && message.contains("workspace-relative directory")
        }));
    }

    #[test]
    fn refuses_a_delivery_action_metadata_file_instead_of_its_directory() {
        let workspace = system_package_workspace("system-package-metadata-action");
        workspace.write(
            ".intentional/config.yml",
            &SYSTEM_PACKAGE_CONFIG.replace(
                ".github/actions/deliver-rpm",
                ".github/actions/deliver-rpm/action.yml",
            ),
        );
        assert!(blocked_diagnostics(&workspace).iter().any(|message| {
            message.contains(".github/actions/deliver-rpm/action.yml")
                && message.contains("workspace-relative directory")
        }));
    }

    #[test]
    fn comparison_revalidates_delivery_metadata_without_workflow_drift() {
        for (label, mutate, expected) in [
            (
                "reserved",
                fn_remove_reserved as fn(String) -> String,
                "release-automation-digest",
            ),
            ("required", fn_add_required, "requires input \"uncovered\""),
            ("kind", fn_change_kind, "runs.using: composite"),
        ] {
            let workspace = system_package_workspace(&format!("system-package-revalidate-{label}"));
            converge(workspace.root(), WorkflowRole::Publish);
            let before = workflow(workspace.root(), WorkflowRole::Publish);
            let path = workspace
                .root()
                .join(".github/actions/deliver-rpm/action.yml");
            let metadata = mutate(std::fs::read_to_string(&path).expect("metadata"));
            workspace.write(".github/actions/deliver-rpm/action.yml", &metadata);
            assert!(blocked_diagnostics(&workspace)
                .iter()
                .any(|message| message.contains(expected)));
            assert_eq!(workflow(workspace.root(), WorkflowRole::Publish), before);
        }
    }

    fn fn_remove_reserved(metadata: String) -> String {
        metadata.replace("  release-automation-digest: {}\n", "")
    }

    fn fn_add_required(metadata: String) -> String {
        metadata.replace("inputs:\n", "inputs:\n  uncovered: { required: true }\n")
    }

    fn fn_change_kind(metadata: String) -> String {
        metadata.replace("using: composite", "using: node20")
    }

    fn run_apt_establishment(
        label: &str,
        name: &str,
        version: &str,
        architecture: &str,
        sealed_bytes: &[u8],
    ) -> (bool, String) {
        let workspace = system_package_workspace(label);
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("workflow");
        let job = &document["jobs"]["release_automation_publish_component_package_apt_primary"];
        let step = job["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Establish "))
            })
            .expect("establishment step");
        let temporary = workspace.root().join("runner");
        let subject = PathBuf::from(
            step["env"]
                .as_mapping()
                .expect("environment")
                .iter()
                .find(|(key, _)| key.as_str().is_some_and(|key| key.ends_with("_SUBJECT")))
                .and_then(|(_, value)| value.as_str())
                .expect("subject path")
                .replace("${{ runner.temp }}", &temporary.display().to_string()),
        );
        std::fs::create_dir_all(&subject).expect("subject");
        let package = subject.join("sample-command_1.2.3.deb");
        std::fs::write(&package, "sealed package bytes").expect("package");
        let digest = crate::evidence::digest_bytes(sealed_bytes);
        let stubs = temporary.join("stubs");
        std::fs::create_dir_all(&stubs).expect("stubs");
        let dpkg = stubs.join("dpkg-deb");
        std::fs::write(&dpkg, "#!/usr/bin/env bash\ncase \"${*: -1}\" in Package) printf '%s\\n' \"${FAKE_NAME}\" ;; Version) printf '%s\\n' \"${FAKE_VERSION}\" ;; Architecture) printf '%s\\n' \"${FAKE_ARCHITECTURE}\" ;; esac\n").expect("stub");
        let status = std::process::Command::new("chmod")
            .args(["+x", dpkg.to_str().expect("path")])
            .status()
            .expect("chmod");
        assert!(status.success());
        let output_file = temporary.join("outputs");
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(step["run"].as_str().expect("run"))
            .current_dir(workspace.root())
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("GITHUB_OUTPUT", &output_file)
            .env("FAKE_NAME", name)
            .env("FAKE_VERSION", version)
            .env("FAKE_ARCHITECTURE", architecture);
        for (key, value) in step["env"].as_mapping().expect("env") {
            let value = value
                .as_str()
                .expect("value")
                .replace("${{ runner.temp }}", &temporary.display().to_string())
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.version }}",
                    "1.2.3",
                )
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.digest }}",
                    &digest,
                );
            command.env(key.as_str().expect("key"), value);
        }
        let output = command.output().expect("establishment runs");
        (
            output.status.success(),
            format!(
                "{}{}",
                std::fs::read_to_string(output_file).unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    }

    #[test]
    fn refuses_a_package_whose_declared_name_disagrees_with_the_release() {
        assert!(
            !run_apt_establishment(
                "system-package-name",
                "other-command",
                "1.2.3",
                "amd64",
                b"sealed package bytes"
            )
            .0
        );
    }

    #[test]
    fn refuses_a_package_whose_declared_version_disagrees_with_the_release() {
        assert!(
            !run_apt_establishment(
                "system-package-version",
                "example-tool",
                "1.2.2",
                "amd64",
                b"sealed package bytes"
            )
            .0
        );
    }

    #[test]
    fn refuses_a_package_whose_bytes_disagree_with_the_sealed_digest() {
        assert!(
            !run_apt_establishment(
                "system-package-digest",
                "example-tool",
                "1.2.3",
                "amd64",
                b"different sealed bytes",
            )
            .0
        );
    }

    #[test]
    fn passes_each_package_declared_architecture_without_comparing_it() {
        for architecture in ["amd64", "arm64"] {
            let (success, outputs) = run_apt_establishment(
                &format!("system-package-architecture-{architecture}"),
                "example-tool",
                "1.2.3",
                architecture,
                b"sealed package bytes",
            );
            assert!(success, "{architecture} establishes: {outputs}");
            assert!(outputs
                .lines()
                .any(|line| line == format!("architecture={architecture}")));
        }
    }

    #[test]
    fn rpm_establishment_compares_the_sealed_version_without_appending_package_release() {
        let workspace = system_package_workspace("system-package-rpm-version");
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("workflow");
        let step = document["jobs"]["release_automation_publish_component_package_rpm_primary"]
            ["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Establish "))
            })
            .expect("establishment step");
        let temporary = workspace.root().join("runner-rpm-establishment");
        let subject = PathBuf::from(
            step["env"]
                .as_mapping()
                .expect("environment")
                .iter()
                .find(|(key, _)| key.as_str().is_some_and(|key| key.ends_with("_SUBJECT")))
                .and_then(|(_, value)| value.as_str())
                .expect("subject path")
                .replace("${{ runner.temp }}", &temporary.display().to_string()),
        );
        std::fs::create_dir_all(&subject).expect("subject");
        std::fs::write(subject.join("example-tool.rpm"), "sealed package bytes").expect("package");
        let digest = crate::evidence::digest_bytes(b"sealed package bytes");
        let stubs = temporary.join("stubs");
        std::fs::create_dir_all(&stubs).expect("stubs");
        let rpm = stubs.join("rpm");
        std::fs::write(
            &rpm,
            "#!/usr/bin/env bash\ncase \"$3\" in '%{NAME}') printf 'example-tool' ;; '%{VERSION}') printf '1.2.3' ;; '%{ARCH}') printf 'arm64' ;; *) exit 2 ;; esac\n",
        )
        .expect("stub");
        assert!(std::process::Command::new("chmod")
            .args(["+x", rpm.to_str().expect("path")])
            .status()
            .expect("chmod")
            .success());
        let output_file = temporary.join("outputs");
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(step["run"].as_str().expect("run"))
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("GITHUB_OUTPUT", &output_file);
        for (key, value) in step["env"].as_mapping().expect("env") {
            let value = value
                .as_str()
                .expect("value")
                .replace("${{ runner.temp }}", &temporary.display().to_string())
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.version }}",
                    "1.2.3",
                )
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.digest }}",
                    &digest,
                );
            command.env(key.as_str().expect("key"), value);
        }
        let output = command.output().expect("establishment runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let outputs = std::fs::read_to_string(output_file).expect("outputs");
        assert!(outputs.lines().any(|line| line == "version=1.2.3"));
        assert!(outputs.lines().any(|line| line == "architecture=arm64"));
    }

    fn run_apt_readback(label: &str, scenario: &str) -> bool {
        let workspace = system_package_workspace(label);
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("workflow");
        let step = document["jobs"]["release_automation_publish_component_package_apt_primary"]
            ["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Read back "))
            })
            .expect("readback step");
        let temporary = workspace.root().join("runner-readback");
        let subject = PathBuf::from(
            step["env"]
                .as_mapping()
                .expect("environment")
                .iter()
                .find(|(key, _)| key.as_str().is_some_and(|key| key.ends_with("_SUBJECT")))
                .and_then(|(_, value)| value.as_str())
                .expect("subject path")
                .replace("${{ runner.temp }}", &temporary.display().to_string()),
        );
        std::fs::create_dir_all(&subject).expect("subject");
        std::fs::write(subject.join("sample-command.deb"), "sealed package bytes")
            .expect("package");
        let digest = crate::evidence::digest_bytes(b"sealed package bytes");
        let packages = if scenario == "package-absent" {
            "Package: another\nVersion: 1.2.3\nArchitecture: amd64\nSHA256: deadbeef\n\n"
        } else {
            "Package: example-tool\nVersion: 1.2.3\nArchitecture: amd64\nSHA256: 4df1176a73c8a18d44f8b4db0df4808205205a5b88c42d36d95321aeecccc213\n\n"
        };
        let packages_digest = crate::evidence::digest_bytes(packages.as_bytes())
            .trim_start_matches("sha256:")
            .to_owned();
        let stubs = temporary.join("stubs");
        std::fs::create_dir_all(&stubs).expect("stubs");
        for (name, body) in [
            ("curl", r#"#!/usr/bin/env bash
set -euo pipefail
output=${*: -1}; url=${*: -3:1}
case "${url}" in
  https://packages.invalid/apt/dists/current/InRelease)
    [ "${FAKE_SCENARIO}" != absent-index ] || exit 22
    digest=${FAKE_PACKAGES_DIGEST}; [ "${FAKE_SCENARIO}" != followed-digest ] || digest=0000000000000000000000000000000000000000000000000000000000000000
    printf 'SHA256:\n %s 1 section-a/binary-amd64/Packages\n' "${digest}" > "${output}" ;;
  https://packages.invalid/apt/dists/current/section-a/binary-amd64/Packages)
    printf '%s' "${FAKE_PACKAGES}" > "${output}" ;;
  https://packages.invalid/apt-key.asc) printf 'key served today' > "${output}" ;;
  *) exit 64 ;;
esac
"#),
            ("gpg", "#!/usr/bin/env bash\nset -euo pipefail\nout=\"$5\"; in=\"$6\"; cp \"${in}\" \"${out}\"\n"),
            ("gpgv", "#!/usr/bin/env bash\n[ \"${FAKE_SCENARIO}\" != bad-signature ]\n"),
            ("dpkg-deb", "#!/usr/bin/env bash\nprintf 'amd64\\n'\n"),
            ("goreleaser", "#!/usr/bin/env bash\nprintf 'goreleaser 2.0\\n'\n"),
            ("apt", "#!/usr/bin/env bash\nprintf 'apt 2.0\\n'\n"),
            ("apt-get", r#"#!/usr/bin/env bash
set -euo pipefail
etc= state= cache=
for argument in "$@"; do
  case "${argument}" in
    Dir::Etc=*) etc=${argument#Dir::Etc=} ;;
    Dir::State=*) state=${argument#Dir::State=} ;;
    Dir::Cache=*) cache=${argument#Dir::Cache=} ;;
  esac
done
test "${etc}" = "${RELEASE_AUTOMATION_WORK}/etc/apt"
test "${state}" = "${RELEASE_AUTOMATION_WORK}/state"
test "${cache}" = "${RELEASE_AUTOMATION_WORK}/cache"
source_line=$(cat "${etc}/sources.list")
test "${source_line}" = "deb [signed-by=${RELEASE_AUTOMATION_WORK}/keyring.gpg] https://packages.invalid/apt current section-a"
if [[ " $* " == *' download '* ]]; then
  if [ "${FAKE_SCENARIO}" = retrieved-mismatch ]; then
    printf 'different retrieved bytes' > retrieved.deb
  else
    printf 'sealed package bytes' > retrieved.deb
  fi
fi
"#),
        ] {
            let path = stubs.join(name);
            std::fs::write(&path, body).expect("stub");
            let status = std::process::Command::new("chmod").args(["+x", path.to_str().expect("path")]).status().expect("chmod");
            assert!(status.success());
        }
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(step["run"].as_str().expect("run"))
            .current_dir(workspace.root())
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("FAKE_SCENARIO", scenario)
            .env("FAKE_PACKAGES", packages)
            .env("FAKE_PACKAGES_DIGEST", packages_digest);
        for (key, value) in step["env"].as_mapping().expect("env") {
            let value = value
                .as_str()
                .expect("value")
                .replace("${{ runner.temp }}", &temporary.display().to_string())
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.version }}",
                    "1.2.3",
                )
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.digest }}",
                    &digest,
                );
            command.env(key.as_str().expect("key"), value);
        }
        command.status().expect("readback runs").success()
    }

    #[test]
    fn apt_readback_accepts_a_matching_signed_index_and_consumer_retrieval() {
        assert!(run_apt_readback("apt-present", "ok"));
    }

    #[test]
    fn apt_readback_refuses_an_absent_index() {
        assert!(!run_apt_readback("apt-absent-index", "absent-index"));
    }

    #[test]
    fn apt_readback_refuses_an_index_with_an_unverified_signature() {
        assert!(!run_apt_readback("apt-bad-signature", "bad-signature"));
    }

    #[test]
    fn apt_readback_refuses_a_followed_digest_that_disagrees() {
        assert!(!run_apt_readback("apt-followed-digest", "followed-digest"));
    }

    #[test]
    fn apt_readback_refuses_a_package_absent_from_the_signed_index() {
        assert!(!run_apt_readback("apt-package-absent", "package-absent"));
    }

    #[test]
    fn apt_readback_refuses_retrieved_bytes_that_disagree_with_the_sealed_subject() {
        assert!(!run_apt_readback(
            "apt-retrieved-mismatch",
            "retrieved-mismatch"
        ));
    }

    fn run_rpm_readback(label: &str, scenario: &str) -> bool {
        let workspace = system_package_workspace(label);
        converge(workspace.root(), WorkflowRole::Publish);
        let document: Value =
            serde_yaml::from_str(&workflow(workspace.root(), WorkflowRole::Publish))
                .expect("workflow");
        let step = document["jobs"]["release_automation_publish_component_package_rpm_primary"]
            ["steps"]
            .as_sequence()
            .expect("steps")
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("Read back "))
            })
            .expect("readback step");
        let temporary = workspace.root().join("runner-rpm-readback");
        let subject = PathBuf::from(
            step["env"]
                .as_mapping()
                .expect("environment")
                .iter()
                .find(|(key, _)| key.as_str().is_some_and(|key| key.ends_with("_SUBJECT")))
                .and_then(|(_, value)| value.as_str())
                .expect("subject path")
                .replace("${{ runner.temp }}", &temporary.display().to_string()),
        );
        std::fs::create_dir_all(&subject).expect("subject");
        std::fs::write(subject.join("example-tool.rpm"), "sealed package bytes").expect("package");
        let digest = crate::evidence::digest_bytes(b"sealed package bytes");
        let package_checksum = digest.trim_start_matches("sha256:");
        let (indexed_name, indexed_version, indexed_architecture, indexed_checksum) = match scenario
        {
            "package-absent" => ("another-tool", "1.2.3", "arm64", package_checksum),
            "version-mismatch" => ("example-tool", "1.2.2", "arm64", package_checksum),
            "architecture-mismatch" => ("example-tool", "1.2.3", "amd64", package_checksum),
            "package-digest-mismatch" => (
                "example-tool",
                "1.2.3",
                "arm64",
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            _ => ("example-tool", "1.2.3", "arm64", package_checksum),
        };
        let primary = format!(
            "<metadata><package><name>{indexed_name}</name><arch>{indexed_architecture}</arch><version ver=\"{indexed_version}\" rel=\"1\"/><checksum>{indexed_checksum}</checksum></package></metadata>"
        );
        let primary_source = temporary.join("primary.xml");
        std::fs::write(&primary_source, &primary).expect("primary source");
        let primary_bytes = if scenario == "gzip-primary" {
            let output = std::process::Command::new("gzip")
                .args(["-n", "-c"])
                .arg(&primary_source)
                .output()
                .expect("gzip runs");
            assert!(output.status.success());
            output.stdout
        } else {
            primary.into_bytes()
        };
        let primary_fixture = temporary.join("primary.fixture");
        std::fs::write(&primary_fixture, &primary_bytes).expect("primary fixture");
        let primary_digest = crate::evidence::digest_bytes(&primary_bytes)
            .trim_start_matches("sha256:")
            .to_owned();
        let stubs = temporary.join("stubs");
        std::fs::create_dir_all(&stubs).expect("stubs");
        for (name, body) in [
            ("curl", r#"#!/usr/bin/env bash
set -euo pipefail
output=${*: -1}; url=${*: -3:1}
case "${url}" in
  https://packages.invalid/rpm/stable/repodata/repomd.xml.asc) printf 'signature' > "${output}" ;;
  https://packages.invalid/rpm/stable/repodata/repomd.xml)
    [ "${FAKE_SCENARIO}" != absent-index ] || exit 22
    digest=${FAKE_PRIMARY_DIGEST}; [ "${FAKE_SCENARIO}" != followed-digest ] || digest=0000000000000000000000000000000000000000000000000000000000000000
    location=metadata/current-primary.xml; [ "${FAKE_SCENARIO}" != alternate-location ] || location=indices/alternate-primary.xml
    printf '<repomd><data type="filelists"><checksum>ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff</checksum><location href="metadata/filelists.xml"/></data><data type="primary"><checksum>%s</checksum><location href="%s"/></data></repomd>' "${digest}" "${location}" > "${output}" ;;
  https://packages.invalid/rpm/stable/metadata/current-primary.xml)
    [ "${FAKE_SCENARIO}" != alternate-location ] || exit 64
    cp "${FAKE_PRIMARY_PATH}" "${output}" ;;
  https://packages.invalid/rpm/stable/indices/alternate-primary.xml)
    [ "${FAKE_SCENARIO}" = alternate-location ] || exit 64
    cp "${FAKE_PRIMARY_PATH}" "${output}" ;;
  https://packages.invalid/rpm-key.asc) printf 'key served today' > "${output}" ;;
  *) exit 64 ;;
esac
"#),
            ("gpg", "#!/usr/bin/env bash\nset -euo pipefail\nout=\"$5\"; in=\"$6\"; cp \"${in}\" \"${out}\"\n"),
            ("gpgv", "#!/usr/bin/env bash\n[ \"${FAKE_SCENARIO}\" != bad-signature ]\n"),
            ("rpm", "#!/usr/bin/env bash\nprintf 'arm64\\n'\n"),
            ("goreleaser", "#!/usr/bin/env bash\nprintf 'goreleaser 2.0\\n'\n"),
            ("dnf", r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = --version ]; then printf 'dnf 4.0\n'; exit 0; fi
test "${1}" = --config
test "${2}" = /dev/null
reposdir= cache= state=
for argument in "$@"; do
  case "${argument}" in
    --setopt=reposdir=*) reposdir=${argument#--setopt=reposdir=} ;;
    --setopt=cachedir=*) cache=${argument#--setopt=cachedir=} ;;
    --setopt=persistdir=*) state=${argument#--setopt=persistdir=} ;;
  esac
done
test "${reposdir}" = "${RELEASE_AUTOMATION_WORK}/etc/yum.repos.d"
test "${cache}" = "${RELEASE_AUTOMATION_WORK}/cache"
test "${state}" = "${RELEASE_AUTOMATION_WORK}/state"
repo=${reposdir}/intentional.repo
test "$(grep -c '^gpgcheck=1$' "${repo}")" -eq 1
test "$(grep -c '^repo_gpgcheck=1$' "${repo}")" -eq 1
grep -Fxq 'baseurl=https://packages.invalid/rpm/stable' "${repo}"
grep -Fxq "gpgkey=file://${RELEASE_AUTOMATION_WORK}/key" "${repo}"
[[ " $* " == *' install example-tool-1.2.3.arm64 '* ]]
if [ "${FAKE_SCENARIO}" = retrieved-mismatch ]; then
  printf 'different retrieved bytes' > "${RELEASE_AUTOMATION_WORK}/retrieved/example-tool.rpm"
else
  printf 'sealed package bytes' > "${RELEASE_AUTOMATION_WORK}/retrieved/example-tool.rpm"
fi
"#),
        ] {
            let path = stubs.join(name);
            std::fs::write(&path, body).expect("stub");
            let status = std::process::Command::new("chmod")
                .args(["+x", path.to_str().expect("path")])
                .status()
                .expect("chmod");
            assert!(status.success());
        }
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(step["run"].as_str().expect("run"))
            .current_dir(workspace.root())
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    test_tool_path(&std::env::var("PATH").unwrap_or_default())
                ),
            )
            .env("FAKE_SCENARIO", scenario)
            .env("FAKE_PRIMARY_PATH", primary_fixture)
            .env("FAKE_PRIMARY_DIGEST", primary_digest);
        for (key, value) in step["env"].as_mapping().expect("env") {
            let value = value
                .as_str()
                .expect("value")
                .replace("${{ runner.temp }}", &temporary.display().to_string())
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.version }}",
                    "1.2.3",
                )
                .replace(
                    "${{ needs.release_automation_build_component_goreleaser.outputs.digest }}",
                    &digest,
                );
            command.env(key.as_str().expect("key"), value);
        }
        command.status().expect("readback runs").success()
    }

    #[test]
    fn rpm_readback_accepts_a_matching_signed_index_and_consumer_retrieval() {
        assert!(run_rpm_readback("rpm-present", "ok"));
    }

    #[test]
    fn rpm_readback_follows_an_alternate_primary_location() {
        assert!(run_rpm_readback(
            "rpm-alternate-location",
            "alternate-location"
        ));
    }

    #[test]
    fn rpm_readback_reads_gzip_compressed_primary_metadata() {
        assert!(run_rpm_readback("rpm-gzip-primary", "gzip-primary"));
    }

    #[test]
    fn rpm_readback_refuses_an_absent_index() {
        assert!(!run_rpm_readback("rpm-absent-index", "absent-index"));
    }

    #[test]
    fn rpm_readback_refuses_an_index_with_an_unverified_signature() {
        assert!(!run_rpm_readback("rpm-bad-signature", "bad-signature"));
    }

    #[test]
    fn rpm_readback_refuses_a_followed_digest_that_disagrees() {
        assert!(!run_rpm_readback("rpm-followed-digest", "followed-digest"));
    }

    #[test]
    fn rpm_readback_refuses_a_package_absent_from_the_signed_index() {
        assert!(!run_rpm_readback("rpm-package-absent", "package-absent"));
    }

    #[test]
    fn rpm_readback_refuses_an_indexed_package_with_a_different_version() {
        assert!(!run_rpm_readback(
            "rpm-version-mismatch",
            "version-mismatch"
        ));
    }

    #[test]
    fn rpm_readback_refuses_an_indexed_package_with_a_different_architecture() {
        assert!(!run_rpm_readback(
            "rpm-architecture-mismatch",
            "architecture-mismatch"
        ));
    }

    #[test]
    fn rpm_readback_refuses_an_indexed_package_with_a_different_digest() {
        assert!(!run_rpm_readback(
            "rpm-package-digest-mismatch",
            "package-digest-mismatch"
        ));
    }

    #[test]
    fn rpm_readback_refuses_retrieved_bytes_that_disagree_with_the_sealed_subject() {
        assert!(!run_rpm_readback(
            "rpm-retrieved-mismatch",
            "retrieved-mismatch"
        ));
    }


    // A tap, a package index, and the Arch User Repository accept no workflow
    // identity token. Granting the scope anyway widens the job for nothing.
    #[test]
    fn withholds_the_workflow_identity_scope_from_repository_destinations() {
        let workspace = go_workspace("workflow-go-permissions");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for publisher in job_ids(&jobs, "intentional_publish_") {
            let permissions = jobs[&Value::String(publisher.clone())]["permissions"]
                .as_mapping()
                .expect("permissions")
                .clone();
            assert_eq!(
                permissions.get(Value::String("contents".to_owned())),
                Some(&Value::String("read".to_owned())),
                "{publisher}"
            );
            assert!(
                permissions
                    .get(Value::String("id-token".to_owned()))
                    .is_none(),
                "{publisher} needs no workflow identity token: {permissions:?}"
            );
        }

        // The scope survives where a registry actually accepts it, so this is a
        // recipe distinction rather than a blanket withdrawal.
        let workspace = two_destination_workspace("workflow-oci-permissions");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for publisher in job_ids(&jobs, "intentional_publish_") {
            assert_eq!(
                jobs[&Value::String(publisher.clone())]["permissions"]["id-token"].as_str(),
                Some("write"),
                "{publisher}"
            );
        }
    }
