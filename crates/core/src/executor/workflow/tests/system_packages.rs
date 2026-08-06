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

    fn cargo_system_package_workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        let common = [
            "intentional-package-path",
            "intentional-format",
            "intentional-name",
            "intentional-version",
            "intentional-architecture",
            "intentional-digest",
        ];
        let mut rpm = common.to_vec();
        rpm.push("intentional-rpm-channel");
        let mut apt = common.to_vec();
        apt.extend(["intentional-apt-suite", "intentional-apt-component"]);
        workspace
            .write(
                "Cargo.toml",
                "[workspace]\nmembers = [\"component\"]\nresolver = \"2\"\n",
            )
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"sample-utility\"\nversion = \"1.2.3\"\ndescription = \"Sample utility\"\nauthors = [\"Release Maintainers <maintainers@example.invalid>\"]\nlicense = \"MIT\"\n",
            )
            .write("component/src/main.rs", "fn main() {}\n")
            .write(
                ".intentional/config.yml",
                r#"$schema: https://intentional.foo/schemas/config.yml
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
      utility:
        path: .
        rpm:
          delivery-action: .github/actions/deliver-rpm
          base-url: https://packages.invalid/rpm
          public-signing-key-url: https://packages.invalid/rpm-key.asc
          observation-deadline: 47
          channel: stable
          with: {}
        apt:
          delivery-action: .github/actions/deliver-apt
          base-url: https://packages.invalid/apt
          public-signing-key-url: https://packages.invalid/apt-key.asc
          observation-deadline: 53
          suite: current
          component: main
          with: {}
        aur: {}
    tags:
      staged: { role: primary, template: '{id}@{version}', require-phase: before-publication }
"#,
            )
            .write(
                ".github/workflows/release.yml",
                "name: repository\n\non:\n  workflow_dispatch:\n\njobs: {}\n",
            )
            .write(
                ".github/workflows/publish.yml",
                "name: repository\n\non:\n  workflow_dispatch:\n\njobs: {}\n",
            )
            .write(
                ".github/actions/deliver-rpm/action.yml",
                &delivery_action(&rpm, "composite"),
            )
            .write(
                ".github/actions/deliver-apt/action.yml",
                &delivery_action(&apt, "composite"),
            );
        workspace
    }

    fn parse_srcinfo(srcinfo: &str) -> (String, BTreeMap<String, Vec<String>>, String) {
        let mut lines = srcinfo.lines();
        let pkgbase = lines
            .next()
            .and_then(|line| line.strip_prefix("pkgbase = "))
            .expect(".SRCINFO opens with its pkgbase identity")
            .to_owned();
        let mut fields = BTreeMap::<String, Vec<String>>::new();
        loop {
            let line = lines.next().expect(".SRCINFO closes its pkgbase section");
            if line.is_empty() {
                break;
            }
            let attribute = line
                .strip_prefix('\t')
                .unwrap_or_else(|| panic!(".SRCINFO attribute uses one tab separator: {line:?}"));
            let (name, value) = attribute
                .split_once(" = ")
                .unwrap_or_else(|| panic!(".SRCINFO attribute names its value: {line:?}"));
            fields
                .entry(name.to_owned())
                .or_default()
                .push(value.to_owned());
        }
        let pkgname = lines
            .next()
            .and_then(|line| line.strip_prefix("pkgname = "))
            .expect(".SRCINFO closes with its package identity")
            .to_owned();
        assert!(lines.next().is_none(), ".SRCINFO carries one package section");
        (pkgbase, fields, pkgname)
    }

    #[cfg(unix)]
    #[test]
    fn cargo_system_routes_build_one_sealed_subject_from_the_open_catalog() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;

        let workspace = cargo_system_package_workspace("cargo-system-route");
        converge(workspace.root(), WorkflowRole::Publish);
        let jobs = publish_jobs(workspace.root());
        let produced_archives = [
            "intentional_build_component_cargo_archive_linux_x86_64",
            "intentional_build_component_cargo_archive_linux_arm64",
            "intentional_build_component_cargo_archive_macos_arm64",
        ]
        .into_iter()
        .map(|producer| {
            let archive = job_steps(&jobs, producer)
                .into_iter()
                .find_map(|step| {
                    step["with"]["path"]
                        .as_str()
                        .and_then(|path| path.strip_prefix("${{ runner.temp }}/"))
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| panic!("{producer} uploads beneath runner.temp"));
            (producer, archive)
        })
        .collect::<BTreeMap<_, _>>();
        let routes = crate::executor::recipe::catalog()
            .iter()
            .filter(|recipe| {
                recipe.capability == Capability::RustCrate
                    && matches!(
                        recipe.publisher,
                        PublisherKind::Rpm | PublisherKind::Apt | PublisherKind::Aur
                    )
            })
            .collect::<Vec<_>>();
        assert_eq!(routes.len(), 3, "the open catalog supplies the route census");
        for route in routes {
            let job = format!(
                "intentional_publish_component_utility_{}_primary",
                route.publisher.as_str()
            );
            let steps = job_steps(&jobs, &job);
            let publisher_body = job_run_bodies(&jobs, &job);
            assert!(!steps.is_empty(), "the {} route derives {job}", route.publisher);
            if matches!(route.publisher, PublisherKind::Rpm | PublisherKind::Apt) {
                for prefix in ["Establish ", "Deliver ", "Read back "] {
                    assert!(
                        steps.iter().any(|step| step["name"]
                            .as_str()
                            .is_some_and(|name| name.starts_with(prefix))),
                        "{job} derives its {prefix} consumer"
                    );
                }
            } else {
                assert!(
                    publisher_body.contains("aur.archlinux.org/${INTENTIONAL_DESTINATION}.git")
                );
                assert!(publisher_body.contains("aur/${INTENTIONAL_DESTINATION}.pkgbuild"));
                let destination = steps
                    .iter()
                    .find_map(|step| step["env"]["INTENTIONAL_DESTINATION"].as_str())
                    .expect("the AUR route carries its derived destination as data");
                assert_eq!(destination, "sample-utility-bin");
            }
            assert!(
                !publisher_body.contains("cargo "),
                "the {} publisher promotes its sealed aggregate without invoking Cargo: {publisher_body}",
                route.publisher
            );
        }

        for id in [
            "intentional_build_component_cargo_archive_linux_x86_64",
            "intentional_build_component_cargo_archive_linux_arm64",
            "intentional_build_component_cargo_archive_macos_arm64",
            "intentional_build_component_cargo_archive",
        ] {
            assert!(jobs.contains_key(id), "the shared subject derives {id}");
        }
        let upload = job_run_bodies(&jobs, "intentional_upload_deliverables");
        for expected in [
            "-name '*.rpm'",
            "-name '*.deb'",
            "! -name '*.deb' ! -name '*.rpm'",
        ] {
            assert!(
                upload.contains(expected),
                "the open route consumers carry {expected}: {upload}"
            );
        }
        let build_steps = job_steps(&jobs, "intentional_build_component_cargo_archive");
        let installer = build_steps
            .iter()
            .find(|step| step["name"].as_str() == Some("Install the pinned nFPM packager"))
            .expect("system routes install their pinned native packager");
        assert_eq!(
            installer["env"]["INTENTIONAL_NFPM_VERSION"].as_str(),
            Some("2.47.0")
        );
        assert_eq!(
            installer["env"]["INTENTIONAL_NFPM_DIGEST"].as_str(),
            Some("0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783")
        );

        let build = build_steps
            .iter()
            .find(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|body| body.contains("linux_x86_64_archive="))
            })
            .expect("aggregate build step");
        let body = build["run"].as_str().expect("aggregate build body").to_owned();
        for (producer, archive) in &produced_archives {
            assert!(
                body.contains(&format!("mv \"${{INTENTIONAL_SUBJECT}}/{archive}\"")),
                "aggregate consumer reads the archive emitted by {producer}: {archive}"
            );
        }
        let execution = workspace.root().join("aggregate-execution");
        let subject = execution.join("subject");
        let archive_fixtures = execution.join("archives");
        let archive_input = execution.join("archive-input");
        std::fs::create_dir_all(&archive_fixtures).expect("archive fixture directory");
        std::fs::create_dir_all(&archive_input).expect("archive input directory");
        for (producer, archive) in &produced_archives {
            std::fs::write(
                archive_input.join("sample-utility"),
                format!("sealed {archive} executable\n"),
            )
            .expect("architecture-specific archive executable");
            let status = std::process::Command::new("tar")
                .args(["-czf"])
                .arg(archive_fixtures.join(archive))
                .arg("-C")
                .arg(&archive_input)
                .arg("sample-utility")
                .status()
                .expect("archive fixture runs");
            assert!(status.success(), "archive fixture creates {archive} for {producer}");
        }
        let stub = execution.join("nfpm");
        std::fs::write(
            &stub,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${NFPM_RECORDING}"
target=""
while [[ $# -gt 0 ]]; do
  if [[ "$1" == --target ]]; then target="$2"; shift 2; else shift; fi
done
test -n "${target}"
install -d "$(dirname "${target}")"
printf 'external package outcome\n' > "${target}"
"#,
        )
        .expect("nFPM outcome stub");
        let mut permissions = std::fs::metadata(&stub).expect("stub metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&stub, permissions).expect("executable stub");
        let recording = execution.join("nfpm.args");
        let run_aggregate = |subject: &std::path::Path, recording: &std::path::Path| {
            std::fs::create_dir_all(subject).expect("subject directory");
            for archive in produced_archives.values() {
                std::fs::copy(archive_fixtures.join(archive), subject.join(archive))
                    .unwrap_or_else(|error| panic!("copy {archive} into subject: {error}"));
            }
            let mut command = std::process::Command::new("bash");
            command
                .arg("-c")
                .arg(&body)
                .current_dir(workspace.root().join("component"))
                .env("GITHUB_REF_NAME", "1.2.3")
                .env("GITHUB_REPOSITORY", "sample-owner/sample-repository")
                .env("RUNNER_TEMP", &execution)
                .env("NFPM_RECORDING", recording)
                .env(
                    "PATH",
                    test_tool_path(&std::env::var("PATH").unwrap_or_default()),
                );
            for (key, value) in step_environment(build) {
                command.env(key, value);
            }
            command
                .env("INTENTIONAL_SUBJECT", subject)
                .env("INTENTIONAL_NFPM", &stub)
                .output()
                .expect("aggregate body runs")
        };
        let output = run_aggregate(&subject, &recording);
        assert!(
            output.status.success(),
            "aggregate body failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let arguments = std::fs::read_to_string(&recording).expect("nFPM calls recorded");
        for expected in [
            "--packager rpm --target",
            "sample-utility-1.2.3.x86_64.rpm",
            "--packager deb --target",
            "sample-utility_1.2.3_amd64.deb",
        ] {
            assert!(arguments.contains(expected), "nFPM calls carry {expected}: {arguments}");
        }
        let nfpm = std::fs::read_to_string(execution.join("intentional_nfpm.yml"))
            .expect("generated nFPM configuration");
        for expected in [
            "name: sample-utility",
            "version: 1.2.3",
            "arch: amd64",
            "maintainer: \"Release Maintainers <maintainers@example.invalid>\"",
            "description: \"Sample utility\"",
            "dst: \"/usr/bin/sample-utility\"",
        ] {
            assert!(nfpm.contains(expected), "nFPM configuration carries {expected}: {nfpm}");
        }
        let pkgbuild = std::fs::read_to_string(subject.join("aur/sample-utility-bin.pkgbuild"))
            .expect("generated PKGBUILD");
        let srcinfo = std::fs::read_to_string(subject.join("aur/sample-utility-bin.srcinfo"))
            .expect("generated .SRCINFO");
        let x86_digest = format!(
            "{:x}",
            Sha256::digest(
                std::fs::read(subject.join("sample-utility-1.2.3-linux-x86_64.tar.gz"))
                    .expect("renamed x86 archive")
            )
        );
        let arm_digest = format!(
            "{:x}",
            Sha256::digest(
                std::fs::read(subject.join("sample-utility-1.2.3-linux-arm64.tar.gz"))
                    .expect("renamed arm archive")
            )
        );
        for expected in [
            "pkgname=sample-utility-bin",
            "pkgver=1.2.3",
            "pkgrel=1",
            "pkgdesc='Sample utility'",
            "arch=('x86_64' 'aarch64')",
            "url='https://github.com/sample-owner/sample-repository'",
            "license=('MIT')",
            "sample-utility-1.2.3-linux-x86_64.tar.gz::https://github.com/sample-owner/sample-repository/releases/download/1.2.3/sample-utility-1.2.3-linux-x86_64.tar.gz",
            "sample-utility-1.2.3-linux-aarch64.tar.gz::https://github.com/sample-owner/sample-repository/releases/download/1.2.3/sample-utility-1.2.3-linux-arm64.tar.gz",
            "install -Dm755 \"${srcdir}/sample-utility\" \"${pkgdir}/usr/bin/sample-utility\"",
        ] {
            assert!(pkgbuild.contains(expected), "PKGBUILD carries {expected}: {pkgbuild}");
        }
        for expected in [
            format!("sha256sums_x86_64=('{}')", x86_digest),
            format!("sha256sums_aarch64=('{}')", arm_digest),
        ] {
            assert!(pkgbuild.contains(&expected), "PKGBUILD carries {expected}: {pkgbuild}");
        }
        let (pkgbase, srcinfo_fields, pkgname) = parse_srcinfo(&srcinfo);
        assert_eq!(pkgbase, "sample-utility-bin");
        assert_eq!(pkgname, "sample-utility-bin");
        let expected_srcinfo_fields = BTreeMap::from([
            ("arch".to_owned(), vec!["x86_64".to_owned(), "aarch64".to_owned()]),
            ("license".to_owned(), vec!["MIT".to_owned()]),
            ("pkgdesc".to_owned(), vec!["Sample utility".to_owned()]),
            ("pkgrel".to_owned(), vec!["1".to_owned()]),
            ("pkgver".to_owned(), vec!["1.2.3".to_owned()]),
            (
                "sha256sums_aarch64".to_owned(),
                vec![arm_digest.clone()],
            ),
            ("sha256sums_x86_64".to_owned(), vec![x86_digest.clone()]),
            (
                "source_aarch64".to_owned(),
                vec!["sample-utility-1.2.3-linux-aarch64.tar.gz::https://github.com/sample-owner/sample-repository/releases/download/1.2.3/sample-utility-1.2.3-linux-arm64.tar.gz".to_owned()],
            ),
            (
                "source_x86_64".to_owned(),
                vec!["sample-utility-1.2.3-linux-x86_64.tar.gz::https://github.com/sample-owner/sample-repository/releases/download/1.2.3/sample-utility-1.2.3-linux-x86_64.tar.gz".to_owned()],
            ),
            (
                "url".to_owned(),
                vec!["https://github.com/sample-owner/sample-repository".to_owned()],
            ),
        ]);
        assert_eq!(srcinfo_fields, expected_srcinfo_fields);

        let manifest_path = workspace.root().join("component/Cargo.toml");
        let licensed_manifest = std::fs::read_to_string(&manifest_path).expect("Cargo manifest");
        let unlicensed_manifest = licensed_manifest.replace("license = \"MIT\"\n", "");
        std::fs::write(&manifest_path, &unlicensed_manifest).expect("license-less Cargo manifest");
        let unlicensed_subject = execution.join("unlicensed-subject");
        let unlicensed_recording = execution.join("unlicensed-nfpm.args");
        let unlicensed_output = run_aggregate(&unlicensed_subject, &unlicensed_recording);
        assert!(
            unlicensed_output.status.success(),
            "license-less aggregate failed: {}",
            String::from_utf8_lossy(&unlicensed_output.stderr)
        );
        let unlicensed_srcinfo = std::fs::read_to_string(
            unlicensed_subject.join("aur/sample-utility-bin.srcinfo"),
        )
        .expect("license-less .SRCINFO");
        let (pkgbase, mut unlicensed_fields, pkgname) = parse_srcinfo(&unlicensed_srcinfo);
        assert_eq!(pkgbase, "sample-utility-bin");
        assert_eq!(pkgname, "sample-utility-bin");
        assert!(
            unlicensed_fields.remove("license").is_none(),
            "license-less .SRCINFO omits the optional field"
        );
        let mut fields_without_license = expected_srcinfo_fields.clone();
        fields_without_license.remove("license");
        assert_eq!(unlicensed_fields, fields_without_license);
        let unlicensed_pkgbuild = std::fs::read_to_string(
            unlicensed_subject.join("aur/sample-utility-bin.pkgbuild"),
        )
        .expect("license-less PKGBUILD");
        assert!(!unlicensed_pkgbuild.lines().any(|line| line.starts_with("license=")));

        let authorless_manifest = unlicensed_manifest.replace(
            "authors = [\"Release Maintainers <maintainers@example.invalid>\"]\n",
            "",
        );
        std::fs::write(&manifest_path, authorless_manifest).expect("author-less Cargo manifest");
        let authorless_subject = execution.join("authorless-subject");
        let authorless_recording = execution.join("authorless-nfpm.args");
        let authorless_output = run_aggregate(&authorless_subject, &authorless_recording);
        assert!(!authorless_output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&authorless_output.stderr),
            "Cargo package sample-utility must declare at least one author before Intentional can derive RPM or APT maintainer metadata\n"
        );
        assert!(
            !subject.join("homebrew").exists(),
            "an unconfigured descriptor route produces no formula"
        );
    }

    #[test]
    fn refuses_rpm_and_apt_routes_without_one_cargo_binary_identity() {
        for (publisher, mapping, action, coordinate) in [
            (
                "rpm",
                "          delivery-action: .github/actions/deliver-rpm\n          base-url: https://packages.invalid/rpm\n          public-signing-key-url: https://packages.invalid/rpm-key.asc\n          observation-deadline: 47\n          channel: stable\n          with: {}\n",
                ".github/actions/deliver-rpm/action.yml",
                "intentional-rpm-channel",
            ),
            (
                "apt",
                "          delivery-action: .github/actions/deliver-apt\n          base-url: https://packages.invalid/apt\n          public-signing-key-url: https://packages.invalid/apt-key.asc\n          observation-deadline: 53\n          suite: current\n          component: main\n          with: {}\n",
                ".github/actions/deliver-apt/action.yml",
                "intentional-apt-suite",
            ),
        ] {
            let workspace = Workspace::new(&format!("cargo-{publisher}-identity-refusal"));
            workspace
                .write(
                    "Cargo.toml",
                    "[workspace]\nmembers = [\"component\"]\nresolver = \"2\"\n",
                )
                .write(
                    "component/Cargo.toml",
                    "[package]\nname = \"sample-library\"\nversion = \"1.0.0\"\n",
                )
                .write("component/src/lib.rs", "pub fn value() -> usize { 1 }\n")
                .write(
                    ".intentional/config.yml",
                    &format!(
                        r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
workspace-tags:
  release: {{ template: '{{version}}' }}
github:
  workflows:
    release: {{ path: .github/workflows/release.yml }}
    publish: {{ path: .github/workflows/publish.yml }}
release-units:
  component:
    path: component
    packages:
      utility:
        path: .
        {publisher}:
{mapping}    tags:
      staged: {{ role: primary, template: '{{id}}@{{version}}', require-phase: before-publication }}
"#
                    ),
                )
                .write(
                    ".github/workflows/release.yml",
                    "name: repository\n\non:\n  workflow_dispatch:\n\njobs: {}\n",
                )
                .write(
                    ".github/workflows/publish.yml",
                    "name: repository\n\non:\n  workflow_dispatch:\n\njobs: {}\n",
                );
            let mut inputs = vec![
                "intentional-package-path",
                "intentional-format",
                "intentional-name",
                "intentional-version",
                "intentional-architecture",
                "intentional-digest",
                coordinate,
            ];
            if publisher == "apt" {
                inputs.push("intentional-apt-component");
            }
            workspace.write(action, &delivery_action(&inputs, "composite"));

            let comparison = compare_workflow(workspace.root(), WorkflowRole::Publish, None)
                .expect("comparison runs");
            assert_eq!(comparison.status, ComparisonStatus::Blocked);
            let diagnostic = comparison
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == "cargo-archive-identity-underived")
                .unwrap_or_else(|| panic!("{publisher} reports its Cargo identity refusal"));
            assert_eq!(
                diagnostic.path.as_deref(),
                Some("release-units.component.packages.utility")
            );
            assert!(
                diagnostic
                    .message
                    .contains("cannot derive one native executable identity"),
                "{publisher} refusal names the missing identity: {diagnostic:?}"
            );
        }
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
