// ---
// relationships:
//   implements: github-release-executor
// ---

// Homebrew route derivation tests moved from `executor::workflow::tests`.

    /// A real Cargo package shape that selects both its registry subject and
    /// its native Homebrew archive subject from the open catalog.
    fn rust_homebrew_workspace(label: &str, manifest: &str) -> Workspace {
        rust_homebrew_workspace_with_package_path(label, manifest, ".")
    }

    fn rust_homebrew_workspace_with_package_path(
        label: &str,
        manifest: &str,
        package_path: &str,
    ) -> Workspace {
        let workspace = Workspace::new(label);
        let package_directory = Path::new("component").join(package_path);
        let workspace_manifest = format!(
            "[workspace]\nmembers = [\"{}\"]\nresolver = \"2\"\n",
            package_directory.display()
        );
        let configuration = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
workspace-tags:
  release: { template: '{version}' }
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    packages:
      command:
        path: @PACKAGE_PATH@
        cargo: {}
        homebrew: { repository: sample-owner/sample-tap }
    tags:
      staged: { role: primary, template: '{id}@{version}', require-phase: before-publication }
"#
        .replace("@PACKAGE_PATH@", package_path);
        workspace
            .write("Cargo.toml", &workspace_manifest)
            .write(".intentional/config.yml", &configuration)
            .write(
                package_directory
                    .join("Cargo.toml")
                    .to_str()
                    .expect("UTF-8 package manifest path"),
                manifest,
            )
            .write(
                ".github/workflows/release.yml",
                "name: repository\n\non:\n  workflow_dispatch:\n\njobs: {}\n",
            )
            .write(
                ".github/workflows/publish.yml",
                "name: repository\n\non:\n  workflow_dispatch:\n\njobs: {}\n",
            );
        workspace
    }

    #[test]
    fn scopes_cargo_archive_build_environment_away_from_buildx() {
        fn build_environment(jobs: &serde_yaml::Mapping, id: &str) -> BTreeSet<String> {
            jobs[id]["steps"]
                .as_sequence()
                .expect("build steps")
                .iter()
                .find(|step| step["env"]["INTENTIONAL_SUBJECT_IDENTITY"].is_string())
                .expect("subject build step")["env"]
                .as_mapping()
                .expect("build environment")
                .keys()
                .map(|name| name.as_str().expect("environment name").to_owned())
                .collect()
        }

        let cargo = rust_homebrew_workspace(
            "workflow-cargo-archive-environment",
            "[package]\nname = \"sample-tool\"\nversion = \"1.2.3\"\n",
        );
        cargo.write("component/src/main.rs", "fn main() {}\n");
        converge(cargo.root(), WorkflowRole::Publish);
        let cargo_environment = build_environment(
            &publish_jobs(cargo.root()),
            "intentional_build_component_cargo_archive",
        );
        let buildx = two_destination_workspace("workflow-buildx-environment");
        converge(buildx.root(), WorkflowRole::Publish);
        let buildx_environment = build_environment(
            &publish_jobs(buildx.root()),
            "intentional_build_component_buildx",
        );

        let shared = [
            "INTENTIONAL_SUBJECT_IDENTITY",
            "INTENTIONAL_TAG_PREFIX",
            "INTENTIONAL_TAG_SUFFIX",
        ];
        let cargo_only = [
            "INTENTIONAL_HOMEBREW",
            "INTENTIONAL_RPM",
            "INTENTIONAL_APT",
            "INTENTIONAL_AUR",
            "INTENTIONAL_AUR_DESTINATION",
            "INTENTIONAL_NFPM",
        ];
        for name in shared {
            assert!(
                cargo_environment.contains(name) && buildx_environment.contains(name),
                "Cargo archive and Buildx build jobs share {name}: cargo={cargo_environment:?}, buildx={buildx_environment:?}"
            );
        }
        for name in cargo_only.iter().copied() {
            assert!(
                cargo_environment.contains(name),
                "the derived Cargo archive build job carries {name}: {cargo_environment:?}"
            );
        }
        for name in cargo_only {
            assert!(
                !buildx_environment.contains(name),
                "the unrelated Buildx build job omits Cargo archive entry {name}: {buildx_environment:?}"
            );
        }
    }

    #[test]
    fn rust_homebrew_builds_platform_archives_once_and_promotes_the_sealed_formula() {
        let workspace = rust_homebrew_workspace(
            "rust-homebrew-route",
            "[package]\nname = \"sample-tool\"\nversion = \"1.2.3\"\ndescription = \"Sample command line tool\"\nlicense = \"MIT\"\n",
        );
        workspace.write("component/src/main.rs", "fn main() {}\n");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());

        for id in [
            "intentional_build_component_cargo_archive_linux_x86_64",
            "intentional_build_component_cargo_archive_linux_arm64",
            "intentional_build_component_cargo_archive_macos_arm64",
            "intentional_build_component_cargo_archive",
            "intentional_publish_component_command_homebrew_primary",
        ] {
            assert!(
                jobs.contains_key(id),
                "the Rust Homebrew route derives {id}"
            );
        }
        let mut produced_archives = BTreeMap::new();
        for (id, artifact, build_tool, target, image) in [
            (
                "intentional_build_component_cargo_archive_linux_x86_64",
                "intentional_archive-component_cargo_archive-linux_x86_64",
                "cross",
                "x86_64-unknown-linux-gnu",
                Some("ghcr.io/cross-rs/x86_64-unknown-linux-gnu:0.2.5@sha256:9e5b39c09874bc1816c675ed11afca2c2ed6cee0c4ed2b3c1d5763c346c9ae3f"),
            ),
            (
                "intentional_build_component_cargo_archive_linux_arm64",
                "intentional_archive-component_cargo_archive-linux_arm64",
                "cross",
                "aarch64-unknown-linux-gnu",
                Some("ghcr.io/cross-rs/aarch64-unknown-linux-gnu:0.2.5@sha256:7f8308a8734d9fcd2ebbe9a3e4bdea74af293f0799d80c3cc341e340cda49a4c"),
            ),
            (
                "intentional_build_component_cargo_archive_macos_arm64",
                "intentional_archive-component_cargo_archive-macos_arm64",
                "cargo",
                "aarch64-apple-darwin",
                None,
            ),
        ] {
            let platform = job_steps(&jobs, id);
            let build = platform
                .iter()
                .find(|step| step["run"].is_string())
                .expect("platform build body");
            let body = build["run"].as_str().expect("platform build script");
            assert!(body.contains(&format!(
                "{build_tool} build --release --locked --target {target}"
            )));
            assert!(body.contains("tar -cf - -C"));
            assert!(!body.contains("--sort=name"));
            let expected_target_dir = format!(
                "${{{{ github.workspace }}}}/target/intentional_cargo-component_cargo_archive-{id_suffix}",
                id_suffix = id
                    .strip_prefix("intentional_build_component_cargo_archive_")
                    .expect("platform suffix")
            );
            assert_eq!(
                build["env"]["CARGO_TARGET_DIR"].as_str(),
                Some(expected_target_dir.as_str())
            );
            let image_value = build["env"]
                .as_mapping()
                .expect("build environment")
                .iter()
                .find_map(|(key, value)| {
                    key.as_str()
                        .filter(|key| key.ends_with("CROSS_IMAGE"))
                        .and_then(|_| value.as_str())
                });
            assert_eq!(image_value, image);
            assert_eq!(
                body.contains("export CROSS_CONFIG=\"${INTENTIONAL_CROSS_CONFIG}\""),
                build_tool == "cross"
            );
            assert_eq!(
                platform.iter().any(|step| {
                    step["uses"].as_str() == Some(CROSS_INSTALL_ACTION)
                        && step["with"]["tool"].as_str() == Some("cross@0.2.5")
                }),
                build_tool == "cross"
            );
            let upload = platform
                .iter()
                .find(|step| step["with"]["name"].as_str() == Some(artifact))
                .unwrap_or_else(|| panic!("{id} uploads artifact {artifact}"));
            let produced_archive = upload["with"]["path"]
                .as_str()
                .and_then(|path| path.strip_prefix("${{ runner.temp }}/"))
                .unwrap_or_else(|| panic!("{id} uploads beneath runner.temp"));
            produced_archives.insert(id, produced_archive.to_owned());
        }
        let platform = job_steps(
            &jobs,
            "intentional_build_component_cargo_archive_linux_x86_64",
        );
        let aggregate = job_steps(&jobs, "intentional_build_component_cargo_archive");
        let aggregate_body = aggregate
            .iter()
            .find_map(|step| step["run"].as_str())
            .expect("aggregate build body");
        for agreement in [
            "linux_x86_64_digest=\"$(sha256sum",
            "linux_arm64_digest=\"$(sha256sum",
            "macos_arm64_digest=\"$(sha256sum",
            "homebrew/Formula/${binary}.rb",
            "${linux_x86_64_digest}",
            "${linux_arm64_digest}",
            "${macos_arm64_digest}",
        ] {
            assert!(
                aggregate_body.contains(agreement),
                "aggregate body carries {agreement}:\n{aggregate_body}"
            );
        }
        for (producer, archive) in &produced_archives {
            let agreement = format!("mv \"${{INTENTIONAL_SUBJECT}}/{archive}\"");
            assert!(
                aggregate_body.contains(&agreement),
                "aggregate consumer reads the archive emitted by {producer}: {archive}\n{aggregate_body}"
            );
        }
        let needs = jobs["intentional_build_component_cargo_archive"]["needs"]
            .as_sequence()
            .expect("aggregate needs");
        for producer in [
            "intentional_build_component_cargo_archive_linux_x86_64",
            "intentional_build_component_cargo_archive_linux_arm64",
            "intentional_build_component_cargo_archive_macos_arm64",
        ] {
            assert!(
                needs.iter().any(|need| need.as_str() == Some(producer)),
                "the sealed aggregate waits for {producer}"
            );
        }
        let download_pattern = aggregate
            .iter()
            .find_map(|step| step["with"]["pattern"].as_str())
            .expect("archive download pattern");
        assert_eq!(
            download_pattern,
            "intentional_archive-component_cargo_archive-*"
        );
        let download = aggregate
            .iter()
            .find(|step| step["with"]["pattern"].as_str() == Some(download_pattern))
            .expect("archive download step");
        let aggregate_build = aggregate
            .iter()
            .find(|step| step["run"].as_str() == Some(aggregate_body))
            .expect("aggregate build step");
        let aggregate_subject = aggregate_build["env"]["INTENTIONAL_SUBJECT"]
            .as_str()
            .expect("aggregate subject path");
        assert_eq!(download["with"]["path"].as_str(), Some(aggregate_subject));
        assert_eq!(download["with"]["merge-multiple"].as_bool(), Some(true));

        let aggregate_root = workspace.root().join("aggregate-execution");
        let subject_root = aggregate_root.join("bytes");
        std::fs::create_dir_all(&subject_root).expect("aggregate subject directory");
        let linux_x86_64_bytes = b"sealed x86-64 Linux archive";
        let linux_arm64_bytes = b"sealed Arm64 Linux archive";
        let macos_arm64_bytes = b"sealed Arm64 macOS archive";
        // Populate the consumer boundary from producer answers. Hand-writing
        // these names would let the fixture ratify a producer/consumer split,
        // just as byte-identical fixtures can ratify a checksum swap.
        for (producer, bytes) in [
            (
                "intentional_build_component_cargo_archive_linux_x86_64",
                linux_x86_64_bytes.as_slice(),
            ),
            (
                "intentional_build_component_cargo_archive_linux_arm64",
                linux_arm64_bytes.as_slice(),
            ),
            (
                "intentional_build_component_cargo_archive_macos_arm64",
                macos_arm64_bytes.as_slice(),
            ),
        ] {
            std::fs::write(subject_root.join(&produced_archives[producer]), bytes)
                .unwrap_or_else(|error| panic!("write {producer} archive: {error}"));
        }
        let mut aggregate_command = std::process::Command::new("bash");
        aggregate_command
            .arg("-c")
            .arg(aggregate_body)
            .current_dir(workspace.root().join("component"))
            .env("GITHUB_REF_NAME", "1.2.3")
            .env("GITHUB_REPOSITORY", "sample-owner/sample-repository")
            .env(
                "PATH",
                test_tool_path(&std::env::var("PATH").unwrap_or_default()),
            );
        for (key, value) in step_environment(
            aggregate
                .iter()
                .find(|step| step["run"].as_str() == Some(aggregate_body))
                .expect("aggregate build step"),
        ) {
            aggregate_command.env(key, value);
        }
        aggregate_command.env("INTENTIONAL_SUBJECT", &subject_root);
        let output = aggregate_command.output().expect("aggregate build runs");
        assert!(
            output.status.success(),
            "aggregate build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let formula = std::fs::read_to_string(subject_root.join("homebrew/Formula/sample-tool.rb"))
            .expect("generated formula");
        let sha256 = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
        for line in [
            "class SampleTool < Formula".to_owned(),
            "desc \"Sample command line tool\"".to_owned(),
            "homepage \"https://github.com/sample-owner/sample-repository\"".to_owned(),
            "license \"MIT\"".to_owned(),
            "version \"1.2.3\"".to_owned(),
            format!(
                "on_linux do\n    on_arm do\n      url \"https://github.com/sample-owner/sample-repository/releases/download/1.2.3/sample-tool-1.2.3-linux-arm64.tar.gz\"\n      sha256 \"{}\"",
                sha256(linux_arm64_bytes)
            ),
            format!(
                "on_intel do\n      url \"https://github.com/sample-owner/sample-repository/releases/download/1.2.3/sample-tool-1.2.3-linux-x86_64.tar.gz\"\n      sha256 \"{}\"",
                sha256(linux_x86_64_bytes)
            ),
            format!(
                "on_macos do\n    on_arm do\n      url \"https://github.com/sample-owner/sample-repository/releases/download/1.2.3/sample-tool-1.2.3-macos-arm64.tar.gz\"\n      sha256 \"{}\"",
                sha256(macos_arm64_bytes)
            ),
            "bin.install \"sample-tool\"".to_owned(),
            "test do\n    assert_match version.to_s, shell_output((bin/\"sample-tool\").to_s + \" --version\")".to_owned(),
        ] {
            assert!(
                formula.contains(&line),
                "formula carries {line}:\n{formula}"
            );
        }

        let digit_root = aggregate_root.join("digit-leading-bytes");
        std::fs::create_dir_all(&digit_root).expect("digit-leading subject directory");
        for (producer, bytes) in [
            (
                "intentional_build_component_cargo_archive_linux_x86_64",
                linux_x86_64_bytes.as_slice(),
            ),
            (
                "intentional_build_component_cargo_archive_linux_arm64",
                linux_arm64_bytes.as_slice(),
            ),
            (
                "intentional_build_component_cargo_archive_macos_arm64",
                macos_arm64_bytes.as_slice(),
            ),
        ] {
            std::fs::write(digit_root.join(&produced_archives[producer]), bytes)
                .unwrap_or_else(|error| panic!("write digit-leading {producer} archive: {error}"));
        }
        let mut digit_command = std::process::Command::new("bash");
        digit_command
            .arg("-c")
            .arg(aggregate_body)
            .current_dir(workspace.root().join("component"))
            .env("GITHUB_REF_NAME", "1.2.3")
            .env("GITHUB_REPOSITORY", "sample-owner/sample-repository")
            .env(
                "PATH",
                test_tool_path(&std::env::var("PATH").unwrap_or_default()),
            );
        for (key, value) in step_environment(
            aggregate
                .iter()
                .find(|step| step["run"].as_str() == Some(aggregate_body))
                .expect("aggregate build step"),
        ) {
            digit_command.env(key, value);
        }
        digit_command
            .env("INTENTIONAL_SUBJECT", &digit_root)
            .env("INTENTIONAL_SUBJECT_IDENTITY", "2fast-tool");
        let output = digit_command
            .output()
            .expect("digit-leading formula builds");
        assert!(output.status.success());
        let digit_formula =
            std::fs::read_to_string(digit_root.join("homebrew/Formula/2fast-tool.rb"))
                .expect("digit-leading formula");
        assert!(
            digit_formula.starts_with("class V2fastTool < Formula\n"),
            "formula class is a Ruby constant: {digit_formula}"
        );
        assert!(
            !digit_formula.contains("license \"\""),
            "formula omits an unavailable license: {digit_formula}"
        );
        assert!(
            !formula.contains("prefix.install_metafiles"),
            "formula does not install metadata absent from its archives: {formula}"
        );
        let publisher = job_steps(
            &jobs,
            "intentional_publish_component_command_homebrew_primary",
        );
        assert!(publisher.iter().any(|step| {
            step["run"]
                .as_str()
                .is_some_and(|body| body.contains("${INTENTIONAL_SUBJECT}/homebrew"))
        }));
        assert!(publisher.iter().all(|step| {
            step["run"]
                .as_str()
                .is_none_or(|body| !body.contains("cargo "))
        }));

        // Execute the product-shaped platform body. The stub replaces only
        // Cargo's external build boundary; tar and gzip create the real archive
        // the generated job uploads.
        let build = platform
            .iter()
            .find(|step| step["run"].is_string())
            .expect("platform build body");
        let temporary = workspace.root().join("platform-execution");
        let stubs = temporary.join("stubs");
        std::fs::create_dir_all(&stubs).expect("stub directory");
        let cargo = stubs.join("cargo");
        std::fs::write(
            &cargo,
            "#!/usr/bin/env bash\nset -euo pipefail\ntarget=''\nwhile [[ $# -gt 0 ]]; do\n  if [[ $1 == --target ]]; then target=$2; shift 2; else shift; fi\ndone\ntest -n \"$target\"\nmkdir -p \"${CARGO_TARGET_DIR}/${target}/release\"\nprintf '#!/usr/bin/env bash\\nprintf \\\"sample-tool 1.2.3\\\\n\\\"\\n' > \"${CARGO_TARGET_DIR}/${target}/release/sample-tool\"\nchmod 755 \"${CARGO_TARGET_DIR}/${target}/release/sample-tool\"\n",
        )
        .expect("Cargo stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755))
                .expect("executable stub");
        }
        let cross = stubs.join("cross");
        std::fs::copy(&cargo, &cross).expect("Cross stub");
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(build["run"].as_str().expect("build script"))
            .current_dir(workspace.root().join("component"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    stubs.display(),
                    test_tool_path(&std::env::var("PATH").unwrap_or_default())
                ),
            )
            .env("RUNNER_TEMP", &temporary);
        for (key, value) in step_environment(build) {
            command.env(key, value);
        }
        command
            .env(
                "CARGO_TARGET_DIR",
                workspace.root().join("target/platform-execution"),
            )
            .env("INTENTIONAL_CROSS_CONFIG", temporary.join("cross.toml"));
        let output = command.output().expect("platform build runs");
        assert!(
            output.status.success(),
            "generated platform build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let archive = temporary.join(
            &produced_archives
                ["intentional_build_component_cargo_archive_linux_x86_64"],
        );
        let listing = std::process::Command::new("tar")
            .args([
                "--full-time",
                "-tvzf",
                archive.to_str().expect("archive path"),
            ])
            .output()
            .expect("archive lists");
        assert!(listing.status.success());
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(
            listing.contains("-rwxr-xr-x"),
            "archive keeps executable mode: {listing}"
        );
        assert!(
            listing.contains("1970-01-01 00:00:00"),
            "archive fixes time: {listing}"
        );
        assert!(
            listing.ends_with(" sample-tool\n"),
            "archive names the binary: {listing}"
        );
        let extracted = temporary.join("extracted");
        std::fs::create_dir_all(&extracted).expect("extraction directory");
        let status = std::process::Command::new("tar")
            .args(["-xzf", archive.to_str().expect("archive path")])
            .current_dir(&extracted)
            .status()
            .expect("archive extracts");
        assert!(status.success());
        let output = std::process::Command::new(extracted.join("sample-tool"))
            .arg("--version")
            .output()
            .expect("archived binary executes");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "sample-tool 1.2.3\n"
        );
    }

    #[test]
    fn rust_homebrew_binds_each_configured_cross_image_to_its_linux_target() {
        let workspace = rust_homebrew_workspace(
            "rust-homebrew-cross-images",
            "[package]\nname = \"sample-tool\"\nversion = \"1.2.3\"\ndescription = \"Sample command line tool\"\nlicense = \"MIT\"\n",
        );
        workspace.write("component/src/main.rs", "fn main() {}\n");
        let config_path = workspace.root().join(".intentional/config.yml");
        let config = std::fs::read_to_string(&config_path).expect("fixture config");
        std::fs::write(
            &config_path,
            config.replace(
                "github:\n",
                "github:\n  cargo-homebrew:\n    linux-x86-64-cross-image: registry.invalid/toolchain/x86@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    linux-arm64-cross-image: registry.invalid/toolchain/arm@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
            ),
        )
        .expect("configure Cross images");

        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        for (job, target, image) in [
            (
                "intentional_build_component_cargo_archive_linux_x86_64",
                "x86_64-unknown-linux-gnu",
                "registry.invalid/toolchain/x86@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (
                "intentional_build_component_cargo_archive_linux_arm64",
                "aarch64-unknown-linux-gnu",
                "registry.invalid/toolchain/arm@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ] {
            let build = job_steps(&jobs, job)
                .into_iter()
                .find(|step| step["run"].is_string())
                .expect("platform build body");
            assert_eq!(
                build["env"]["INTENTIONAL_CROSS_IMAGE"].as_str(),
                Some(image),
                "{target} consumes its configured image"
            );
            let body = build["run"].as_str().expect("build body");
            assert!(
                body.contains(&format!("'[target.%s]\\nimage = \\\"%s\\\"\\n' {target}")),
                "{target} writes its configured image to CROSS_CONFIG:\n{body}"
            );
        }
    }

    #[test]
    fn rust_homebrew_defaults_each_omitted_cross_image_independently() {
        let configured_x86 = "registry.invalid/toolchain/x86@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let configured_arm = "registry.invalid/toolchain/arm@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let default_x86 = "ghcr.io/cross-rs/x86_64-unknown-linux-gnu:0.2.5@sha256:9e5b39c09874bc1816c675ed11afca2c2ed6cee0c4ed2b3c1d5763c346c9ae3f";
        let default_arm = "ghcr.io/cross-rs/aarch64-unknown-linux-gnu:0.2.5@sha256:7f8308a8734d9fcd2ebbe9a3e4bdea74af293f0799d80c3cc341e340cda49a4c";
        for (label, setting, expected_x86, expected_arm) in [
            (
                "rust-homebrew-only-x86-image",
                format!("    linux-x86-64-cross-image: {configured_x86}\n"),
                configured_x86,
                default_arm,
            ),
            (
                "rust-homebrew-only-arm-image",
                format!("    linux-arm64-cross-image: {configured_arm}\n"),
                default_x86,
                configured_arm,
            ),
        ] {
            let workspace = rust_homebrew_workspace(
                label,
                "[package]\nname = \"sample-tool\"\nversion = \"1.2.3\"\ndescription = \"Sample command line tool\"\nlicense = \"MIT\"\n",
            );
            workspace.write("component/src/main.rs", "fn main() {}\n");
            let config_path = workspace.root().join(".intentional/config.yml");
            let config = std::fs::read_to_string(&config_path).expect("fixture config");
            std::fs::write(
                &config_path,
                config.replace(
                    "github:\n",
                    &format!("github:\n  cargo-homebrew:\n{setting}"),
                ),
            )
            .expect("configure one Cross image");

            converge(workspace.root(), WorkflowRole::Publish);
            let jobs = publish_jobs(workspace.root());
            for (job, expected) in [
                (
                    "intentional_build_component_cargo_archive_linux_x86_64",
                    expected_x86,
                ),
                (
                    "intentional_build_component_cargo_archive_linux_arm64",
                    expected_arm,
                ),
            ] {
                let build = job_steps(&jobs, job)
                    .into_iter()
                    .find(|step| step["run"].is_string())
                    .expect("platform build body");
                assert_eq!(
                    build["env"]["INTENTIONAL_CROSS_IMAGE"].as_str(),
                    Some(expected),
                    "{label} derives {job} independently"
                );
            }
        }
    }

    #[test]
    fn rust_homebrew_refusal_names_the_package_and_undetermined_formula() {
        for (label, manifest) in [
            (
                "rust-homebrew-library-refusal",
                "[package]\nname = \"sample-library\"\nversion = \"1.2.3\"\n",
            ),
            (
                "rust-homebrew-multiple-binaries-refusal",
                "[package]\nname = \"sample-cli\"\nversion = \"1.2.3\"\n\n[[bin]]\nname = \"first\"\npath = \"src/first.rs\"\n\n[[bin]]\nname = \"second\"\npath = \"src/second.rs\"\n",
            ),
        ] {
            let workspace = rust_homebrew_workspace(label, manifest);
            let config = Config::load(workspace.root()).expect("configuration loads");
            let github = config.github.as_ref().expect("GitHub configuration");
            let diagnostics = derive_contract(
                workspace.root(),
                &config,
                github,
                WorkflowRole::Publish,
            )
            .expect_err("the package determines no single Homebrew formula");
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "homebrew-formula-underived")
                .expect("named formula refusal");
            assert_eq!(
                diagnostic.path.as_deref(),
                Some("release-units.component.packages.command.homebrew")
            );
            assert!(diagnostic
                .message
                .contains("component/command/homebrew/primary"));
            assert!(diagnostic.message.contains("component/Cargo.toml"));
            assert!(diagnostic.message.contains("exactly one [[bin]].name"));
        }
    }

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
