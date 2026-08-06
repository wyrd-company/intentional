// ---
// relationships:
//   implements: github-release-executor
// ---

// Packager-specific build environment derivation moved from `executor::workflow`.

use super::*;
use crate::config::CargoHomebrewConfig;

/// Values one packager's build command reads from its step environment.
///
/// Buildx annotates the index it seals. Cargo archive names the formula and
/// removes the global tag's literal affixes to obtain its version. The subject
/// name and tag affixes reach both bodies as data rather than as source.
pub(super) fn build_environment(subject: &DistinctSubject, global_tag: &str) -> String {
    if !matches!(subject.packager, Packager::Buildx | Packager::CargoArchive) {
        return String::new();
    }
    let (prefix, suffix) = global_tag
        .split_once("{version}")
        .unwrap_or((global_tag, ""));
    format!(
        "      @ENVVAR@SUBJECT_IDENTITY: {}\n      @ENVVAR@TAG_PREFIX: {}\n      @ENVVAR@TAG_SUFFIX: {}\n",
        scalar(&subject.identity),
        scalar(prefix),
        scalar(suffix),
    )
}

/// Platform builds whose archives become one sealed Homebrew subject.
pub(super) fn cargo_archive_platforms(
    subject: &DistinctSubject,
    namespaces: &PrefixNamespaces,
    verify: &str,
    config: &CargoHomebrewConfig,
) -> Vec<(String, std::result::Result<Value, WorkflowDiagnostic>)> {
    let x86_environment = format!(
        "      @ENVVAR@CROSS_CONFIG: ${{{{ runner.temp }}}}/intentional-cross.toml\n      @ENVVAR@CROSS_IMAGE: {}\n",
        config.linux_x86_64_cross_image
    );
    let arm_environment = format!(
        "      @ENVVAR@CROSS_CONFIG: ${{{{ runner.temp }}}}/intentional-cross.toml\n      @ENVVAR@CROSS_IMAGE: {}\n",
        config.linux_arm64_cross_image
    );
    [
        (
            "linux_x86_64",
            "ubuntu-latest",
            "linux-x86-64.tar.gz",
            "x86_64-unknown-linux-gnu",
            "cross",
            "  - name: Install the pinned Cross packager\n    uses: @CROSS_INSTALL@\n    with:\n      tool: cross@0.2.5\n",
            x86_environment.as_str(),
            "      printf '[target.%s]\\nimage = \\\"%s\\\"\\n' @TARGET@ \"${@ENVVAR@CROSS_IMAGE}\" > \"${@ENVVAR@CROSS_CONFIG}\"\n      export CROSS_CONFIG=\"${@ENVVAR@CROSS_CONFIG}\"\n",
        ),
        (
            "linux_arm64",
            "ubuntu-latest",
            "linux-arm64.tar.gz",
            "aarch64-unknown-linux-gnu",
            "cross",
            "  - name: Install the pinned Cross packager\n    uses: @CROSS_INSTALL@\n    with:\n      tool: cross@0.2.5\n",
            arm_environment.as_str(),
            "      printf '[target.%s]\\nimage = \\\"%s\\\"\\n' @TARGET@ \"${@ENVVAR@CROSS_IMAGE}\" > \"${@ENVVAR@CROSS_CONFIG}\"\n      export CROSS_CONFIG=\"${@ENVVAR@CROSS_CONFIG}\"\n",
        ),
        (
            "macos_arm64",
            "macos-14",
            "macos-arm64.tar.gz",
            "aarch64-apple-darwin",
            "cargo",
            "",
            "",
            "",
        ),
    ]
    .into_iter()
    .map(
        |(
            platform,
            runner,
            archive,
            target,
            build_tool,
            platform_toolchain,
            platform_env,
            build_setup,
        )| {
            let id = format!("{}build_{}_{}", namespaces.job, subject.slug, platform);
            let build_setup = build_setup.replace("@TARGET@", target);
            let rendered = job(
                templates::PUBLISH_CARGO_ARCHIVE_PLATFORM_JOB,
                namespaces,
                &[
                    ("@NEEDS@", &render_list(&[verify.to_owned()])),
                    ("@RUNNER@", runner),
                    ("@PLATFORM@", platform),
                    ("@WORKING_DIRECTORY@", &scalar(&subject.working_directory)),
                    ("@SUBJECT_IDENTITY@", &scalar(&subject.identity)),
                    ("@ARCHIVE@", archive),
                    ("@SLUG@", &subject.slug),
                    ("@TARGET@", target),
                    ("@BUILD_TOOL@", build_tool),
                    ("@PLATFORM_TOOLCHAIN@", platform_toolchain),
                    ("@PLATFORM_ENV@", platform_env),
                    ("@BUILD_SETUP@", &build_setup),
                ],
            );
            (id, rendered)
        },
    )
    .collect()
}
