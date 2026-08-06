// ---
// relationships:
//   validates: github-release-executor
// ---

use serde_yaml::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn write_executable(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("write executable stub");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("executable stub permissions");
}

/// The shipped Action installer must stop before extracting an archive whose
/// bytes disagree with the release checksum it downloaded.
#[test]
fn action_installer_refuses_a_tampered_release_archive_at_its_checksum_guard() {
    let temporary = tempfile::tempdir().expect("temporary Action installer root");
    let root = temporary.path();
    let bin = root.join("bin");
    let payload = root.join("payload");
    fs::create_dir_all(&bin).expect("stub directory");
    fs::create_dir_all(&payload).expect("payload directory");

    let architecture = Command::new("uname")
        .arg("-m")
        .output()
        .expect("read runner architecture");
    assert!(architecture.status.success(), "uname succeeds");
    let asset = match String::from_utf8(architecture.stdout)
        .expect("architecture is text")
        .trim()
    {
        "x86_64" => "intentional-linux-x86_64.tar.gz",
        "aarch64" | "arm64" => "intentional-linux-arm64.tar.gz",
        other => panic!("the shipped Action supports the test architecture {other}"),
    };
    let archive_root = asset.trim_end_matches(".tar.gz");
    let archive_directory = payload.join(archive_root);
    fs::create_dir_all(&archive_directory).expect("archive directory");
    write_executable(
        &archive_directory.join("intentional"),
        "#!/usr/bin/env bash\nexit 0\n",
    );
    let archive = root.join(asset);
    let packed = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .args(["-C"])
        .arg(&payload)
        .arg(archive_root)
        .status()
        .expect("create valid archive");
    assert!(packed.success(), "the tampered fixture remains extractable");

    let checksums = root.join("SHA256SUMS");
    fs::write(
        &checksums,
        format!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  {asset}\n"),
    )
    .expect("write a well-formed checksum for different bytes");
    write_executable(
        &bin.join("curl"),
        r#"#!/usr/bin/env bash
set -euo pipefail
output=""
while (($#)); do
  if [[ "$1" == "--output" ]]; then
    output="$2"
    shift 2
  else
    shift
  fi
done
if [[ "$output" == */SHA256SUMS ]]; then
  cp "$INSTALLER_CHECKSUM_FIXTURE" "$output"
else
  cp "$INSTALLER_ARCHIVE_FIXTURE" "$output"
fi
"#,
    );

    let github_path = root.join("github-path");
    fs::write(&github_path, "").expect("GitHub path file");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new("bash")
        .arg("../../scripts/action/install-intentional.sh")
        .arg("1.2.3")
        .env("PATH", path)
        .env("RUNNER_TEMP", root.join("runner"))
        .env("GITHUB_PATH", github_path)
        .env("INSTALLER_ARCHIVE_FIXTURE", &archive)
        .env("INSTALLER_CHECKSUM_FIXTURE", &checksums)
        .output()
        .expect("execute shipped Action installer");
    assert!(
        !output.status.success(),
        "a valid but checksum-mismatched archive is refused"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Checksum verification failed"),
        "the archive dies at the checksum comparison:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pinned_gnu_task_executes_shared_all_target_and_doctest_contracts() {
    let taskfile = fs::read_to_string("../../Taskfile.yml").expect("Taskfile is readable");
    let taskfile: Value = serde_yaml::from_str(&taskfile).expect("Taskfile parses");
    let command = taskfile["tasks"]["pinned-gnu:test"]["cmds"][0]
        .as_str()
        .expect("pinned GNU task has one shell body");
    for required in [
        ". scripts/release/linux-gnu-baseline.env",
        "scripts/ci/install-workflow-test-tools.sh .ci-tools/bin",
        ". .ci-tools/bin/workflow-test-tools.sh",
        "cross test $GNU_CROSS_TEST_ARGUMENTS",
        "cross test $GNU_CROSS_DOC_TEST_ARGUMENTS",
    ] {
        assert!(
            command.lines().any(|line| line.trim() == required),
            "pinned GNU task executes the shared contract line: {required}"
        );
    }

    let baseline = fs::read_to_string("../../scripts/release/linux-gnu-baseline.env")
        .expect("GNU baseline is readable");
    for required in [
        "GNU_CROSS_PASSTHROUGH_ENVIRONMENT=\"ACTIONLINT JQ PYTHON_HOME SHELLCHECK RUSTC_WRAPPER GNU_BUILD_GIT_VERSION GNU_BUILD_BASH_VERSION\"",
        "GNU_CROSS_VOLUME_ENVIRONMENT=\"ACTIONLINT JQ PYTHON_HOME SHELLCHECK RUSTC_WRAPPER\"",
        "GNU_CROSS_TEST_ARGUMENTS=\"--workspace --locked --all-targets --no-fail-fast --target x86_64-unknown-linux-gnu\"",
        "GNU_CROSS_DOC_TEST_ARGUMENTS=\"--workspace --locked --doc --no-fail-fast --target x86_64-unknown-linux-gnu\"",
    ] {
        assert!(
            baseline.lines().any(|line| line == required),
            "GNU baseline declares the complete pinned contract: {required}"
        );
    }

    let temporary = tempfile::tempdir().expect("temporary pinned GNU task root");
    let root = temporary.path();
    fs::create_dir_all(root.join("scripts/release")).expect("baseline directory");
    fs::create_dir_all(root.join("scripts/ci")).expect("installer directory");
    fs::create_dir_all(root.join("bin")).expect("stub bin directory");
    fs::write(
        root.join("scripts/release/linux-gnu-baseline.env"),
        concat!(
            "RUST_VERSION=consumer-toolchain\n",
            "GNU_BUILD_GIT_VERSION=2.7.4\n",
            "GNU_BUILD_BASH_VERSION=4.3.48\n",
            "GNU_CROSS_PASSTHROUGH_ENVIRONMENT='ACTIONLINT JQ PYTHON_HOME SHELLCHECK RUSTC_WRAPPER GNU_BUILD_GIT_VERSION GNU_BUILD_BASH_VERSION'\n",
            "GNU_CROSS_VOLUME_ENVIRONMENT='ACTIONLINT JQ PYTHON_HOME SHELLCHECK RUSTC_WRAPPER'\n",
            "GNU_BASELINE_SOURCE_COUNT=$((${GNU_BASELINE_SOURCE_COUNT:-0} + 1))\n",
            "if [[ $GNU_BASELINE_SOURCE_COUNT -eq 1 ]]; then\n",
            "  GNU_CROSS_TEST_ARGUMENTS='--consumer-all-targets'\n",
            "  GNU_CROSS_DOC_TEST_ARGUMENTS='--consumer-doc-tests'\n",
            "else\n",
            "  GNU_CROSS_TEST_ARGUMENTS='--installer-all-targets'\n",
            "  GNU_CROSS_DOC_TEST_ARGUMENTS='--installer-doc-tests'\n",
            "fi\n",
        ),
    )
    .expect("write sentinel baseline");
    fs::write(
        root.join("Cross.toml"),
        concat!(
            "[target.x86_64-unknown-linux-gnu]\n",
            "passthrough = [\"ACTIONLINT\", \"JQ\", \"PYTHON_HOME\", \"SHELLCHECK\", \"RUSTC_WRAPPER\", \"GNU_BUILD_GIT_VERSION\", \"GNU_BUILD_BASH_VERSION\"]\n",
            "volumes = [\"ACTIONLINT\", \"JQ\", \"PYTHON_HOME\", \"SHELLCHECK\", \"RUSTC_WRAPPER\"]\n",
        ),
    )
    .expect("write sentinel Cross configuration");
    fs::copy(
        "../../scripts/ci/install-workflow-test-tools.sh",
        root.join("scripts/ci/install-workflow-test-tools.sh"),
    )
    .expect("copy production installer into controlled root");

    write_executable(
        &root.join("bin/curl"),
        r#"#!/usr/bin/env bash
set -euo pipefail
while (($#)); do
  if [[ "$1" == -o ]]; then
    output="$2"
    break
  fi
  shift
done
if [[ "$output" == *jq-linux-amd64 ]]; then
  cat > "$output" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --version ]]; then
  echo jq-1.7.1
  exit 0
fi
while (($#)); do
  if [[ "$1" == --arg && "$2" == value ]]; then
    printf "'%s'\n" "$3"
    exit 0
  fi
  shift
done
exit 1
EOF
  chmod 0755 "$output"
else
  : > "$output"
fi
"#,
    );
    write_executable(
        &root.join("bin/sha256sum"),
        "#!/usr/bin/env bash\nset -euo pipefail\ncat >/dev/null\n",
    );
    write_executable(
        &root.join("bin/uname"),
        "#!/usr/bin/env bash\nset -euo pipefail\necho x86_64\n",
    );
    write_executable(
        &root.join("bin/tar"),
        r#"#!/usr/bin/env bash
set -euo pipefail
arguments="$*"
destination=
while (($#)); do
  if [[ "$1" == -C ]]; then
    destination="$2"
    break
  fi
  shift
done
case "$arguments" in
  *actionlint*)
    mkdir -p "$destination"
    printf '#!/usr/bin/env bash\necho actionlint 1.7.7\n' > "$destination/actionlint"
    chmod 0755 "$destination/actionlint"
    ;;
  *cpython*)
    mkdir -p "$destination/python/bin"
    printf '#!/usr/bin/env bash\necho Python 3.12.13\n' > "$destination/python/bin/python3"
    chmod 0755 "$destination/python/bin/python3"
    ;;
  *shellcheck*)
    mkdir -p "$destination/shellcheck-v0.10.0"
    printf '#!/usr/bin/env bash\necho ShellCheck 0.10.0\n' > "$destination/shellcheck-v0.10.0/shellcheck"
    chmod 0755 "$destination/shellcheck-v0.10.0/shellcheck"
    ;;
esac
"#,
    );

    let cross = root.join("bin/cross");
    write_executable(
        &cross,
        concat!(
            "#!/usr/bin/env bash\n",
            "set -euo pipefail\n",
            "printf '%s|%s|%s|%s|%s\\n' \"$*\" \"$ACTIONLINT\" \"$JQ\" ",
            "\"$PYTHON_HOME/bin/python3\" \"$SHELLCHECK\" >> \"$PINNED_GNU_INVOCATIONS\"\n",
        ),
    );

    let invocations = root.join("cross-invocations");
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("stub PATH");
    let output = Command::new("bash")
        .args(["-c", command])
        .current_dir(root)
        .env("PATH", &path)
        .env("PINNED_GNU_INVOCATIONS", &invocations)
        .output()
        .expect("execute pinned GNU task body");
    assert!(
        output.status.success(),
        "pinned GNU task body succeeds with controlled consumers:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(invocations).expect("Cross invocation log"),
        format!(
            concat!(
                "test --installer-all-targets|{0}/.ci-tools/bin/actionlint|{0}/.ci-tools/bin/jq|{0}/.ci-tools/bin/python/bin/python3|{0}/.ci-tools/bin/shellcheck\n",
                "test --installer-doc-tests|{0}/.ci-tools/bin/actionlint|{0}/.ci-tools/bin/jq|{0}/.ci-tools/bin/python/bin/python3|{0}/.ci-tools/bin/shellcheck\n",
            ),
            root.display(),
        ),
        "installer emission is the last writer for all-target and doctest arguments"
    );
    let hosted_environment = fs::read_to_string(root.join(".ci-tools/bin/workflow-test-tools.env"))
        .expect("hosted workflow environment");
    assert!(
        hosted_environment
            .lines()
            .any(|line| line == "GNU_CROSS_TEST_ARGUMENTS=--installer-all-targets"),
        "hosted installer environment exports the all-target argument contract"
    );
    assert!(
        !hosted_environment
            .lines()
            .any(|line| line.starts_with("GNU_CROSS_DOC_TEST_ARGUMENTS=")),
        "hosted installer environment does not export unread doctest arguments"
    );

    let real_sha256sum = Command::new("sh")
        .args(["-c", "command -v sha256sum"])
        .output()
        .expect("locate real sha256sum");
    assert!(
        real_sha256sum.status.success(),
        "real sha256sum is available"
    );
    let real_sha256sum = String::from_utf8(real_sha256sum.stdout)
        .expect("sha256sum path is UTF-8")
        .trim()
        .to_owned();
    write_executable(
        &root.join("bin/sha256sum"),
        r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
if [[ "$input" == *"$TAMPERED_WORKFLOW_TOOL_ASSET"* ]]; then
  printf '%s\n' "$input" | "$REAL_SHA256SUM" "$@"
fi
"#,
    );
    for tampered_asset in [
        "actionlint_1.7.7_linux_amd64.tar.gz",
        "jq-linux-amd64",
        "cpython-3.12.13+20260805-x86_64-unknown-linux-gnu-install_only.tar.gz",
        "shellcheck-v0.10.0.linux.x86_64.tar.xz",
    ] {
        let destination = root.join("tampered").join(tampered_asset);
        let output = Command::new("bash")
            .args([
                "scripts/ci/install-workflow-test-tools.sh",
                destination.to_str().expect("tampered destination is UTF-8"),
            ])
            .current_dir(root)
            .env("PATH", &path)
            .env("REAL_SHA256SUM", &real_sha256sum)
            .env("TAMPERED_WORKFLOW_TOOL_ASSET", tampered_asset)
            .output()
            .expect("execute installer with tampered workflow tool payload");
        assert!(
            !output.status.success(),
            "installer digest guard refuses tampered {tampered_asset} payload"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("{tampered_asset}: FAILED")),
            "tampered {tampered_asset} payload dies at its sha256sum check:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn cargo_homebrew_compatibility_task_is_part_of_repository_validation() {
    let taskfile = fs::read_to_string("../../Taskfile.yml").expect("Taskfile is readable");
    let taskfile: Value = serde_yaml::from_str(&taskfile).expect("Taskfile parses");
    let dependencies = taskfile["tasks"]["ci"]["deps"]
        .as_sequence()
        .expect("local CI dependencies");
    assert!(
        dependencies
            .iter()
            .any(|dependency| dependency.as_str() == Some("cargo-homebrew:compatibility")),
        "local CI executes the Cargo/Homebrew compatibility gate"
    );
    let compatibility_command =
        taskfile["tasks"]["cargo-homebrew:compatibility"]["cmds"][0].as_str();
    assert_eq!(
        compatibility_command,
        Some(
            "cargo test -p intentional-core cargo_homebrew_platform_contract -- --ignored --nocapture"
        ),
        "the documented gate selects the four product-shaped platform contract tests"
    );

    let workflow =
        fs::read_to_string("../../.github/workflows/ci.yml").expect("CI workflow is readable");
    let workflow: Value = serde_yaml::from_str(&workflow).expect("CI workflow parses");
    let hosted_steps = workflow["jobs"]["test"]["steps"]
        .as_sequence()
        .expect("hosted test steps");
    let compatibility_step = hosted_steps
        .iter()
        .position(|step| {
            step["name"].as_str() == Some("Exercise Cargo Homebrew emitted bodies")
                && step["env"]["LC_ALL"].as_str() == Some("C")
                && step["run"].as_str() == compatibility_command
        })
        .expect("hosted CI executes the documented Cargo/Homebrew compatibility command");
    let workspace_test_step = hosted_steps
        .iter()
        .position(|step| step["run"].as_str() == Some("cargo test --workspace"))
        .expect("hosted CI executes aggregate workspace tests");
    assert!(
        compatibility_step < workspace_test_step,
        "hosted Cargo/Homebrew compatibility runs before aggregate workspace tests"
    );
}
