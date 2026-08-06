// ---
// relationships:
//   validates: github-release-executor
// ---

use serde_yaml::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

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
            "GNU_CROSS_TEST_ARGUMENTS='--consumer-all-targets'\n",
            "GNU_CROSS_DOC_TEST_ARGUMENTS='--consumer-doc-tests'\n",
        ),
    )
    .expect("write sentinel baseline");

    let installer = root.join("scripts/ci/install-workflow-test-tools.sh");
    fs::write(
        &installer,
        concat!(
            "#!/usr/bin/env bash\n",
            "set -euo pipefail\n",
            "mkdir -p \"$1\"\n",
            "cat > \"$1/workflow-test-tools.sh\" <<'EOF'\n",
            "ACTIONLINT=/tools/actionlint\n",
            "JQ=/tools/jq\n",
            "PYTHON_HOME=/tools/python\n",
            "SHELLCHECK=/tools/shellcheck\n",
            "RUSTC_WRAPPER=/tools/pinned-gnu-rustc-wrapper\n",
            "EOF\n",
        ),
    )
    .expect("write installer stub");
    fs::set_permissions(&installer, fs::Permissions::from_mode(0o755))
        .expect("installer stub is executable");

    let cross = root.join("bin/cross");
    fs::write(
        &cross,
        concat!(
            "#!/usr/bin/env bash\n",
            "set -euo pipefail\n",
            "printf '%s|%s|%s|%s|%s\\n' \"$*\" \"$ACTIONLINT\" \"$JQ\" ",
            "\"$PYTHON_HOME/bin/python3\" \"$SHELLCHECK\" >> \"$PINNED_GNU_INVOCATIONS\"\n",
        ),
    )
    .expect("write Cross stub");
    fs::set_permissions(&cross, fs::Permissions::from_mode(0o755))
        .expect("Cross stub is executable");

    let invocations = root.join("cross-invocations");
    let path = std::env::join_paths(std::iter::once(root.join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("stub PATH");
    let output = Command::new("bash")
        .args(["-c", command])
        .current_dir(root)
        .env("PATH", path)
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
        concat!(
            "test --consumer-all-targets|/tools/actionlint|/tools/jq|/tools/python/bin/python3|/tools/shellcheck\n",
            "test --consumer-doc-tests|/tools/actionlint|/tools/jq|/tools/python/bin/python3|/tools/shellcheck\n",
        ),
        "pinned GNU task executes all targets and doctests with every provisioned runtime tool"
    );
}
