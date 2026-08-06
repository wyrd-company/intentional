// ---
// relationships:
//   implements: github-release-executor
// ---

//! Managed job templates and the substitution that renders them.
//!
//! The derivation module decides which jobs exist, what they depend on, and
//! what values fill them. This module holds the bodies those decisions are
//! filled into, the pinned Action and packager identities the bodies name, and
//! the substitution that turns one body plus one set of derived values into a
//! parsed job.
//!
//! Every managed checkout states `fetch-tags: true` rather than inheriting tags
//! from `fetch-depth: 0`. The portable commands these jobs invoke derive
//! version authority and resolve the global release tag from the repository's
//! tags, so anyone shortening the fetch depth to speed a job up would otherwise
//! silently remove a guarantee the release protocol depends on.
//!
//! Every `intentional` command a template runs is an argument-level contract
//! with the command-line interface that nothing else in this module checks:
//! workflow derivation never consults the argument parser, and a workflow
//! linter validates syntax and action references rather than the semantics of a
//! `run:` body. The command-line crate parses every generated invocation with
//! the real parser for that reason, so a renamed flag or a changed positional
//! fails there rather than in a privileged job on a release runner.
//!
//! The rationale cannot live in the emitted workflow: each template is parsed
//! into a value before it is spliced, and the editor has no comment support, so
//! managed jobs are emitted comment-free by construction.

use crate::config::PrefixNamespaces;
use crate::executor::recipe::Packager;
use serde_yaml::Value;

use super::{WorkflowDiagnostic, OWNERSHIP_SENTINEL, WORKFLOW_CONTRACT};

pub(super) const CHECKOUT_ACTION: &str =
    "actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09";
pub(super) const UPLOAD_ARTIFACT_ACTION: &str =
    "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02";
pub(super) const DOWNLOAD_ARTIFACT_ACTION: &str =
    "actions/download-artifact@634f93cb2916e3fdff6788551b99b062d0335ce0";

/// Action installing the GoReleaser command a stock runner does not carry.
pub(super) const GORELEASER_INSTALL_ACTION: &str =
    "goreleaser/goreleaser-action@f06c13b6b1a9625abc9e6e439d9c05a8f2190e94";
/// Installer for the Cross version bound to the Linux GNU baseline.
pub(super) const CROSS_INSTALL_ACTION: &str =
    "taiki-e/install-action@cb33e69fad06166ca28a42b2575e4dadabf62ee8";

/// Multi-platform emulation a container-driver Buildx build needs.
pub(super) const SETUP_QEMU_ACTION: &str =
    "docker/setup-qemu-action@96fe6ef7f33517b61c61be40b68a1882f3264fb8";
/// Container-driver Buildx builder, which a stock runner does not start with.
pub(super) const SETUP_BUILDX_ACTION: &str =
    "docker/setup-buildx-action@bb05f3f5519dd87d3ba754cc423b652a5edd6d2c";
/// Registry client every OCI recipe reads and promotes with.
pub(in crate::executor) const SETUP_CRANE_ACTION: &str =
    "imjasonh/setup-crane@feee3b6bb0d4c68370f256a4502498c9227e5c6b";
/// Keyless signing client an OCI recipe installs only when it signs.
pub(in crate::executor) const COSIGN_INSTALLER_ACTION: &str =
    "sigstore/cosign-installer@6f9f17788090df1f26f669e9d70d6ae9567deba6";

/// GoReleaser release the maintained Go recipes are written against.
///
/// The packager is pinned for the reason every external Action is pinned, and
/// then one more. These recipes do not merely run the packager; they read what
/// it wrote, at paths and under names the packager decides: `homebrew/
/// <directory>/<name>.rb`, `aur/<package>.pkgbuild`, the `-bin` suffix the Arch
/// pipe adds, the `nfpms` formats. Every one of those is a claim about a
/// particular GoReleaser, so leaving the installer's default floating would let
/// a layout change reach a release runner as a promotion that finds nothing.
///
/// A floating version also costs reproducibility: the same source would not
/// build the same subject twice once the packager moved underneath it.
pub(super) const GORELEASER_VERSION: &str = "2.17.1";

/// nFPM release that turns a sealed Cargo executable into RPM and Debian packages.
pub(super) const NFPM_VERSION: &str = "2.47.0";
/// SHA-256 digest of nFPM's Linux x86-64 archive for [`NFPM_VERSION`].
pub(super) const NFPM_LINUX_X86_64_DIGEST: &str =
    "0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783";

pub(super) const APP_TOKEN_ACTION: &str =
    "actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349";

/// Repository publishing Intentional's own thin, credential-free Actions.
pub(super) const ACTION_REPOSITORY: &str = "wyrd-company/intentional";

/// Reference a managed job resolves one of Intentional's own Actions at.
///
/// External Actions are pinned to complete commit identities because their
/// contents are outside this project's control. Intentional's own Actions are
/// pinned to the released version that derived the workflow, and that same
/// version is what each Action installs, so one derivation names one Action
/// revision and one binary revision and the two cannot drift apart. A commit
/// identity cannot serve here: derivation must name a revision that will carry
/// the release this build belongs to, and that commit does not exist yet.
pub(super) fn action_reference(name: &str) -> String {
    format!("{ACTION_REPOSITORY}/actions/{name}@{}", crate::VERSION)
}

/// OCI annotation and Dockerfile label naming a runnable image.
pub(super) const OCI_TITLE_LABEL: &str = "org.opencontainers.image.title";

/// Steps installing the packager the build job runs.
///
/// A stock runner already carries the toolchains its hosted images ship, so most
/// packagers need nothing here. GoReleaser is not one of them: it is a separate
/// command, the build job is the sole producer of every Go deliverable the
/// publisher jobs promote, and a build job that cannot run its packager produces
/// nothing at all.
///
/// The installer is pinned to a complete commit identity for the same reason
/// every other external Action is, and it installs only the command: the release
/// itself is driven by the build step, which is where the graph can see it.
pub(super) const fn toolchain_steps(packager: Packager) -> &'static str {
    match packager {
        Packager::GoReleaser => {
            "  - name: Install the GoReleaser packager\n    uses: @GORELEASER_INSTALL@\n    with:\n      install-only: true\n      version: @GORELEASER_VERSION@\n"
        }
        Packager::CargoArchive => {
            "  - name: Download the sealed native archives\n    uses: @DOWNLOAD@\n    with:\n      pattern: @JOB@archive-@SLUG@-*\n      path: ${{ runner.temp }}/@JOB@subject/@SLUG@/bytes\n      merge-multiple: true\n"
        }
        // A stock runner's default Buildx builder uses the docker driver, which
        // can neither emit an OCI layout nor build more than the runner's own
        // platform. Both are requirements of the subject this job seals, so the
        // container-driver builder and its emulation are part of the recipe
        // rather than an optimization.
        Packager::Buildx => {
            "  - name: Enable multi-platform image builds\n    uses: @SETUP_QEMU@\n  - name: Start a container-driver Buildx builder\n    uses: @SETUP_BUILDX@\n"
        }
        Packager::Npm | Packager::Cargo | Packager::DevContainerCli => "",
    }
}

/// Install the pinned nFPM command used only by Cargo system-package routes.
pub(super) fn nfpm_toolchain_steps() -> String {
    format!(
        r#"  - name: Install the pinned nFPM packager
    env:
      @ENVVAR@NFPM_VERSION: {NFPM_VERSION}
      @ENVVAR@NFPM_DIGEST: {NFPM_LINUX_X86_64_DIGEST}
    run: |
      set -euo pipefail
      archive="${{RUNNER_TEMP}}/@JOB@nfpm.tar.gz"
      install -d "${{RUNNER_TEMP}}/@JOB@tools"
      curl -fsSL --max-redirs 5 \
        "https://github.com/goreleaser/nfpm/releases/download/v${{@ENVVAR@NFPM_VERSION}}/nfpm_${{@ENVVAR@NFPM_VERSION}}_Linux_x86_64.tar.gz" \
        -o "${{archive}}"
      printf '%s  %s\n' "${{@ENVVAR@NFPM_DIGEST}}" "${{archive}}" | sha256sum --check
      tar -xzf "${{archive}}" -C "${{RUNNER_TEMP}}/@JOB@tools" nfpm
"#
    )
}

/// Native command that produces one subject's bytes without distributing them.
///
/// This is the packager seam the maintained recipes refine. It states the
/// minimal native build each packager performs and where the graph expects its
/// bytes; provenance generation, attached metadata, signatures, and destination
/// alias behaviour belong to the recipe that owns the destination, not here.
pub(super) fn build_command(packager: Packager) -> String {
    match packager {
        Packager::Npm => "      npm pack --pack-destination \"${@ENVVAR@SUBJECT}\"".to_owned(),
        Packager::Cargo => {
            "      cargo package --locked --target-dir \"${RUNNER_TEMP}/@JOB@cargo\"\n      cp \"${RUNNER_TEMP}\"/@JOB@cargo/package/*.crate \"${@ENVVAR@SUBJECT}/\"".to_owned()
        }
        Packager::CargoArchive => format!(
            r#"{RELEASE_VERSION_COMMAND}      binary="${{@ENVVAR@SUBJECT_IDENTITY}}"
      linux_x86_64_archive="${{binary}}-${{version}}-linux-x86_64.tar.gz"
      linux_arm64_archive="${{binary}}-${{version}}-linux-arm64.tar.gz"
      macos_arm64_archive="${{binary}}-${{version}}-macos-arm64.tar.gz"
      mv "${{@ENVVAR@SUBJECT}}/{linux_x86_64_input}" "${{@ENVVAR@SUBJECT}}/${{linux_x86_64_archive}}"
      mv "${{@ENVVAR@SUBJECT}}/{linux_arm64_input}" "${{@ENVVAR@SUBJECT}}/${{linux_arm64_archive}}"
      mv "${{@ENVVAR@SUBJECT}}/{macos_arm64_input}" "${{@ENVVAR@SUBJECT}}/${{macos_arm64_archive}}"
      linux_x86_64_digest="$(sha256sum "${{@ENVVAR@SUBJECT}}/${{linux_x86_64_archive}}" | cut -d' ' -f1)"
      linux_arm64_digest="$(sha256sum "${{@ENVVAR@SUBJECT}}/${{linux_arm64_archive}}" | cut -d' ' -f1)"
      macos_arm64_digest="$(sha256sum "${{@ENVVAR@SUBJECT}}/${{macos_arm64_archive}}" | cut -d' ' -f1)"
      metadata="$(cargo metadata --no-deps --format-version 1)"
      description="$(jq -r --arg name "${{binary}}" '.packages[] | select(any(.targets[]; .name == $name and any(.kind[]; . == "bin"))) | .description // "Native executable"' <<<"${{metadata}}")"
      license="$(jq -r --arg name "${{binary}}" '.packages[] | select(any(.targets[]; .name == $name and any(.kind[]; . == "bin"))) | .license // empty' <<<"${{metadata}}")"
      description_literal="$(jq -Rn --arg value "${{description}}" '$value')"
      license_literal="$(jq -Rn --arg value "${{license}}" '$value')"
      license_line=""
      if [[ -n "${{license}}" ]]; then license_line="  license ${{license_literal}}"; fi
      if [[ "${{@ENVVAR@HOMEBREW}}" == true ]]; then
        formula_class="$(printf '%s' "${{binary}}" | awk -F '[-_]' '{{ for (i=1; i<=NF; i++) printf toupper(substr($i,1,1)) substr($i,2) }}')"
        if [[ "${{formula_class}}" == [0-9]* ]]; then formula_class="V${{formula_class}}"; fi
        formula="${{@ENVVAR@SUBJECT}}/homebrew/Formula/${{binary}}.rb"
        install -d "$(dirname "${{formula}}")"
        printf '%s\n' \
          "class ${{formula_class}} < Formula" \
          "  desc ${{description_literal}}" \
          "  homepage \"https://github.com/${{GITHUB_REPOSITORY}}\"" \
          "${{license_line}}" \
          "  version \"${{version}}\"" \
          "  on_linux do" \
          "    on_arm do" \
          "      url \"https://github.com/${{GITHUB_REPOSITORY}}/releases/download/${{GITHUB_REF_NAME}}/${{linux_arm64_archive}}\"" \
          "      sha256 \"${{linux_arm64_digest}}\"" \
          "    end" \
          "    on_intel do" \
          "      url \"https://github.com/${{GITHUB_REPOSITORY}}/releases/download/${{GITHUB_REF_NAME}}/${{linux_x86_64_archive}}\"" \
          "      sha256 \"${{linux_x86_64_digest}}\"" \
          "    end" \
          "  end" \
          "  on_macos do" \
          "    on_arm do" \
          "      url \"https://github.com/${{GITHUB_REPOSITORY}}/releases/download/${{GITHUB_REF_NAME}}/${{macos_arm64_archive}}\"" \
          "      sha256 \"${{macos_arm64_digest}}\"" \
          "    end" \
          "  end" \
          "  def install" \
          "    bin.install \"${{binary}}\"" \
          "  end" \
          "  test do" \
          "    assert_match version.to_s, shell_output((bin/\"${{binary}}\").to_s + \" --version\")" \
          "  end" \
          "end" > "${{formula}}"
      fi
      if [[ "${{@ENVVAR@RPM}}" == true || "${{@ENVVAR@APT}}" == true ]]; then
        package_root="${{RUNNER_TEMP}}/@JOB@package-root"
        rm -rf "${{package_root}}"
        install -d "${{package_root}}/usr/bin"
        tar -xzf "${{@ENVVAR@SUBJECT}}/${{linux_x86_64_archive}}" \
          -C "${{package_root}}/usr/bin" "${{binary}}"
        source_literal="$(jq -Rn --arg value "${{package_root}}/usr/bin/${{binary}}" '$value')"
        destination_literal="$(jq -Rn --arg value "/usr/bin/${{binary}}" '$value')"
        nfpm_config="${{RUNNER_TEMP}}/@JOB@nfpm.yml"
        printf '%s\n' \
          "name: ${{binary}}" \
          "arch: amd64" \
          "platform: linux" \
          "version: ${{version}}" \
          "maintainer: Intentional <releases@intentional.foo>" \
          "description: ${{description_literal}}" \
          "license: ${{license_literal}}" \
          "contents:" \
          "  - src: ${{source_literal}}" \
          "    dst: ${{destination_literal}}" > "${{nfpm_config}}"
        if [[ "${{@ENVVAR@RPM}}" == true ]]; then
          "${{@ENVVAR@NFPM}}" package --config "${{nfpm_config}}" --packager rpm \
            --target "${{@ENVVAR@SUBJECT}}/${{binary}}-${{version}}.x86_64.rpm"
        fi
        if [[ "${{@ENVVAR@APT}}" == true ]]; then
          "${{@ENVVAR@NFPM}}" package --config "${{nfpm_config}}" --packager deb \
            --target "${{@ENVVAR@SUBJECT}}/${{binary}}_${{version}}_amd64.deb"
        fi
      fi
      if [[ "${{@ENVVAR@AUR}}" == true ]]; then
        pkgname="${{@ENVVAR@AUR_DESTINATION}}"
        test -n "${{pkgname}}"
        pkgver="${{version//-/_}}"
        description_shell="$(jq -Rr '@sh' <<<"${{description//$'\n'/ }}")"
        license_shell="$(jq -Rr '@sh' <<<"${{license}}")"
        license_pkgbuild=""
        license_srcinfo=""
        if [[ -n "${{license}}" ]]; then
          license_pkgbuild="license=(${{license_shell}})"
          license_srcinfo="\tlicense = ${{license}}"
        fi
        x86_source="${{binary}}-${{version}}-linux-x86_64.tar.gz"
        arm64_source="${{binary}}-${{version}}-linux-aarch64.tar.gz"
        base_url="https://github.com/${{GITHUB_REPOSITORY}}/releases/download/${{GITHUB_REF_NAME}}"
        pkgbuild="${{@ENVVAR@SUBJECT}}/aur/${{pkgname}}.pkgbuild"
        srcinfo="${{@ENVVAR@SUBJECT}}/aur/${{pkgname}}.srcinfo"
        install -d "$(dirname "${{pkgbuild}}")"
        printf '%s\n' \
          "pkgname=${{pkgname}}" \
          "pkgver=${{pkgver}}" \
          "pkgrel=1" \
          "pkgdesc=${{description_shell}}" \
          "arch=('x86_64' 'aarch64')" \
          "url='https://github.com/${{GITHUB_REPOSITORY}}'" \
          "${{license_pkgbuild}}" \
          "source_x86_64=('${{x86_source}}::${{base_url}}/${{linux_x86_64_archive}}')" \
          "source_aarch64=('${{arm64_source}}::${{base_url}}/${{linux_arm64_archive}}')" \
          "sha256sums_x86_64=('${{linux_x86_64_digest}}')" \
          "sha256sums_aarch64=('${{linux_arm64_digest}}')" \
          "package() {{" \
          "  install -Dm755 \"\${{srcdir}}/${{binary}}\" \"\${{pkgdir}}/usr/bin/${{binary}}\"" \
          "}}" > "${{pkgbuild}}"
        printf '%s\n' \
          "pkgbase = ${{pkgname}}" \
          "\tpkgdesc = ${{description//$'\n'/ }}" \
          "\tpkgver = ${{pkgver}}" \
          "\tpkgrel = 1" \
          "\turl = https://github.com/${{GITHUB_REPOSITORY}}" \
          "\tarch = x86_64" \
          "\tarch = aarch64" \
          "${{license_srcinfo}}" \
          "\tsource_x86_64 = ${{x86_source}}::${{base_url}}/${{linux_x86_64_archive}}" \
          "\tsha256sums_x86_64 = ${{linux_x86_64_digest}}" \
          "\tsource_aarch64 = ${{arm64_source}}::${{base_url}}/${{linux_arm64_archive}}" \
          "\tsha256sums_aarch64 = ${{linux_arm64_digest}}" \
          "" \
          "pkgname = ${{pkgname}}" > "${{srcinfo}}"
      fi"#,
            linux_x86_64_input = super::build::CARGO_ARCHIVE_LINUX_X86_64,
            linux_arm64_input = super::build::CARGO_ARCHIVE_LINUX_ARM64,
            macos_arm64_input = super::build::CARGO_ARCHIVE_MACOS_ARM64,
        ),
        Packager::GoReleaser => {
            "      goreleaser release --clean --skip=publish,announce\n      cp -R dist/. \"${@ENVVAR@SUBJECT}/\"".to_owned()
        }
        // The index annotations are part of the bytes the release seals, so
        // they are attached here rather than added at a destination later:
        // mutating them at promotion time would give each destination a subject
        // the seal never saw. Revision is the released commit and created is
        // that commit's timestamp, both read from the checkout rather than from
        // the runner's clock, so the same commit annotates the same way on
        // every rerun.
        Packager::Buildx => format!(
            "{RELEASE_VERSION_COMMAND}      created=\"$(git show -s --format=%cI HEAD)\"\n      docker buildx build \\\n        --platform {OCI_IMAGE_PLATFORMS} \\\n        --provenance true \\\n        --sbom true \\\n        --annotation \"index:{OCI_TITLE_LABEL}=${{@ENVVAR@SUBJECT_IDENTITY}}\" \\\n        --annotation \"index:org.opencontainers.image.version=${{version}}\" \\\n        --annotation \"index:org.opencontainers.image.revision=${{GITHUB_SHA}}\" \\\n        --annotation \"index:org.opencontainers.image.created=${{created}}\" \\\n        --annotation \"index:org.opencontainers.image.source=${{GITHUB_SERVER_URL}}/${{GITHUB_REPOSITORY}}\" \\\n        --output \"type=oci,dest=${{@ENVVAR@SUBJECT}}/{OCI_LAYOUT_ARCHIVE}\" ."
        ),
        Packager::DevContainerCli => format!(
            "      npm install --global --no-fund --no-audit --ignore-scripts {DEV_CONTAINER_CLI}\n      devcontainer features package --output-folder \"${{@ENVVAR@SUBJECT}}\" ."
        ),
    }
}

/// Platform matrix every maintained Buildx recipe produces.
///
/// The matrix belongs to the recipe rather than to configuration: it is what
/// makes one subject resolvable from both destinations for both architectures,
/// and a per-repository matrix would let two destinations of one subject
/// disagree about what the release contains.
pub(super) const OCI_IMAGE_PLATFORMS: &str = "linux/amd64,linux/arm64";

/// Name the Buildx recipe writes its OCI layout archive under.
pub(super) const OCI_LAYOUT_ARCHIVE: &str = "subject.oci.tar";

/// Dev Container CLI revision the maintained build command drives.
pub(super) const DEV_CONTAINER_CLI: &str = "@devcontainers/cli@0.88.0";

/// Shell assigning `version` from the global release tag this run was triggered by.
///
/// The tag's template is derivation-time knowledge, so the extraction is exact
/// rather than a pattern guess. The affixes are routed through `env:` rather
/// than spliced, because a configured template is repository text and this body
/// runs beside the checkout.
pub(super) const RELEASE_VERSION_COMMAND: &str = r#"      version="${GITHUB_REF_NAME}"
      version="${version#"${@ENVVAR@TAG_PREFIX}"}"
      version="${version%"${@ENVVAR@TAG_SUFFIX}"}"
"#;

/// Build one platform archive that the aggregate Cargo archive subject seals.
pub(super) const PUBLISH_CARGO_ARCHIVE_PLATFORM_JOB: &str = r#"
needs:
@NEEDS@
runs-on: @RUNNER@
permissions:
  contents: read
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
@PLATFORM_TOOLCHAIN@  - name: Build the @PLATFORM@ native executable
    working-directory: @WORKING_DIRECTORY@
    env:
      @ENVVAR@SUBJECT_IDENTITY: @SUBJECT_IDENTITY@
      @ENVVAR@ARCHIVE: @ARCHIVE@
      CARGO_TARGET_DIR: ${{ github.workspace }}/target/@JOB@cargo-@SLUG@-@PLATFORM@
@PLATFORM_ENV@    run: |
      set -euo pipefail
@BUILD_SETUP@      @BUILD_TOOL@ build --release --locked --target @TARGET@ \
        --bin "${@ENVVAR@SUBJECT_IDENTITY}"
      binary_path="${CARGO_TARGET_DIR}/@TARGET@/release/${@ENVVAR@SUBJECT_IDENTITY}"
      chmod 755 "${binary_path}"
      touch -t 197001010000 "${binary_path}"
      tar -cf - -C "$(dirname "${binary_path}")" \
        "${@ENVVAR@SUBJECT_IDENTITY}" \
        | gzip -n > "${RUNNER_TEMP}/${@ENVVAR@ARCHIVE}"
  - name: Upload the @PLATFORM@ native archive
    uses: @UPLOAD@
    with:
      name: @JOB@archive-@SLUG@-@PLATFORM@
      path: ${{ runner.temp }}/@ARCHIVE@
      retention-days: 1
"#;

pub(super) fn render_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("  - {value}\n"))
        .collect()
}

/// Render one value as a YAML scalar that cannot alter the template's shape.
pub(in crate::executor) fn scalar(value: &str) -> String {
    serde_yaml::to_string(&Value::String(value.to_owned()))
        .unwrap_or_else(|_| format!("{value:?}"))
        .trim_end()
        .to_owned()
}

/// Substitute one template's derived values, in the order the caller states.
///
/// The substitution table is held to the template in both directions, and only
/// one of the two directions is obvious. A placeholder a template names and
/// nothing substitutes reaches the emitted document as a literal `@NAME@`,
/// which is loud. A substitution entry naming a placeholder the template does
/// not carry is silent: `replace` on an absent needle is a no-op, so the entry
/// is dead and reads as live. Two such entries accumulated in one job's list
/// within a day of each other, both orphaned when a template the derivation
/// stopped rendering was collapsed into a shared one. This refuses the entry
/// instead, at the point where the template and the list are both in hand.
///
/// The second refusal is what keeps the list's order from becoming load
/// bearing. A value substituted at one position is still subject to every
/// entry behind it, so a value that happened to contain a later entry's
/// placeholder would be rewritten by it -- and moving either entry would then
/// change a rendered byte. No derived value names another derived placeholder
/// today; refusing one is what keeps that a property rather than a
/// coincidence, and it is why recipe-emitted steps may be substituted at any
/// position without carrying an ordering contract of their own.
///
/// Namespace and pinned-identity placeholders are deliberately outside both
/// rules: every template shares one table for those, so an entry no single
/// template names is expected, and a derived value naming one is the whole
/// reason they are substituted last.
fn substituted(
    template: &str,
    extra: &[(&str, &str)],
) -> std::result::Result<String, WorkflowDiagnostic> {
    let mut rendered = template.to_owned();
    for (index, (placeholder, value)) in extra.iter().enumerate() {
        if !rendered.contains(placeholder) {
            return Err(WorkflowDiagnostic::new(
                "job-substitution-unnamed",
                format!(
                    "a managed job template is substituted for {placeholder}, which no template it renders names"
                ),
            ));
        }
        if let Some((later, _)) = extra[index + 1..]
            .iter()
            .find(|(later, _)| value.contains(later))
        {
            return Err(WorkflowDiagnostic::new(
                "job-substitution-ordered",
                format!(
                    "the value substituted for {placeholder} names {later}, which a later substitution would rewrite"
                ),
            ));
        }
        rendered = rendered.replace(placeholder, value);
    }
    Ok(rendered)
}

/// Parse a managed job template after substituting its derived values.
///
/// Every substituted value that lands in a scalar position is rendered through
/// [`scalar`], and a template that still does not parse is reported rather than
/// panicking: these bodies carry repository-write authority, so an unexpected
/// configuration-derived id must fail loudly, not silently reshape a job.
pub(super) fn job(
    template: &str,
    namespaces: &PrefixNamespaces,
    extra: &[(&str, &str)],
) -> std::result::Result<Value, WorkflowDiagnostic> {
    // Derived values are substituted first because a value can itself name a
    // namespace placeholder: a packager's build script refers to the prefixed
    // subject environment variable, and substituting the namespaces first would
    // leave that reference unrendered in a privileged job.
    let rendered = substituted(template, extra)?;
    let rendered = rendered
        .replace("@JOB@", &namespaces.job)
        .replace("@ENVVAR@", &namespaces.envvar)
        .replace("@ENVIRONMENT@", &namespaces.environment)
        .replace("@SENTINEL@", OWNERSHIP_SENTINEL)
        .replace("@CONTRACT@", WORKFLOW_CONTRACT)
        .replace("@CHECKOUT@", CHECKOUT_ACTION)
        .replace("@UPLOAD@", UPLOAD_ARTIFACT_ACTION)
        .replace("@DOWNLOAD@", DOWNLOAD_ARTIFACT_ACTION)
        .replace("@APP_TOKEN@", APP_TOKEN_ACTION)
        .replace("@GORELEASER_INSTALL@", GORELEASER_INSTALL_ACTION)
        .replace("@CROSS_INSTALL@", CROSS_INSTALL_ACTION)
        .replace("@SETUP_QEMU@", SETUP_QEMU_ACTION)
        .replace("@SETUP_BUILDX@", SETUP_BUILDX_ACTION)
        .replace("@GORELEASER_VERSION@", &scalar(GORELEASER_VERSION))
        .replace("@VERSION@", &scalar(crate::VERSION))
        .replace("@PREPARE_ACTION@", &action_reference("prepare-release"))
        .replace(
            "@VERIFY_HANDOFF_ACTION@",
            &action_reference("verify-handoff"),
        )
        .replace(
            "@VERIFY_RELEASE_TAG_ACTION@",
            &action_reference("verify-release-tag"),
        )
        .replace(
            "@VERIFY_PUBLICATION_ACTION@",
            &action_reference("verify-publication"),
        )
        .replace("@ASSEMBLE_ACTION@", &action_reference("assemble-evidence"))
        .replace(
            "@BUILT_SUBJECT_ACTION@",
            &action_reference("record-built-subject"),
        )
        .replace("@TAG_PHASE_ACTION@", &action_reference("seal-phase-tags"));
    serde_yaml::from_str(&rendered).map_err(|error| {
        WorkflowDiagnostic::new(
            "job-template-invalid",
            format!("a managed job template did not render to valid YAML: {error}"),
        )
    })
}

/// Render one repeated step template that is spliced into a job template.
///
/// The upload job's per-deliverable and per-handoff steps carry substitution
/// lists of their own and are rendered before the job that holds them, so they
/// are held to the same rules for the same reason: a dead entry in one of these
/// lists is exactly as silent as a dead entry in a job's.
pub(super) fn step(
    template: &str,
    extra: &[(&str, &str)],
) -> std::result::Result<String, WorkflowDiagnostic> {
    substituted(template, extra)
}

/// The managed job templates themselves.
///
/// Each body is a complete job. A placeholder is either a namespace or a
/// pinned identity, which [`job`] substitutes for every template alike, or a
/// derived value the caller supplies for this template only.
pub(super) const RELEASE_PREPARE_JOB: &str = r#"
runs-on: ubuntu-latest
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the accepted source commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Construct the release candidate
    uses: @PREPARE_ACTION@
    with:
      output: ${{ runner.temp }}/@JOB@candidate
      intentional-version: @VERSION@
  - name: Upload the release candidate
    uses: @UPLOAD@
    with:
      name: @JOB@candidate
      path: ${{ runner.temp }}/@JOB@candidate
      retention-days: 1
"#;

/// The authority transition, which also creates the draft the publication
/// protocol depends on.
///
/// Creation belongs here rather than in a job of its own because this job
/// already holds the installation token and is the last managed job that runs
/// before the pushed tag can trigger publication. A separate creator would mint
/// repository authority a second time to write a Release the transition could
/// have written while its own token was still live.
///
/// Creation follows the atomic push in the same job for a reason a reader can
/// otherwise talk themselves out of: a draft cannot be created for a tag the
/// remote does not carry, and `--verify-tag` is what turns a reordering into a
/// failure on the runner rather than a Release attached to nothing.
///
/// Creation is create-if-absent, and absent means absent. `gh release view`
/// fails for a Release that does not exist and equally for a rate limit, a 5xx,
/// or a revoked token, so the branch is taken only when the failure says the
/// Release was not found. Reading every failure as absence would turn the one
/// case create-if-absent exists to serve -- a rerun where the draft does exist
/// -- into a hard failure whose message points away from the real cause.
///
/// `immutable-github-release` states that a failure before closure leaves a
/// resumable draft, so a rerun of this job has to find that draft and continue.
/// A Release that exists and is no longer a draft is the opposite case: the
/// release already closed, and continuing would mean uploading assets onto an
/// immutable Release, so the transition refuses.
///
/// The two streams are kept apart on the success path. `gh` writes notices,
/// deprecations and its own update prompt to standard error on calls that exit
/// zero, so a resolution that merged them would make those bytes part of the
/// value it compares and refuse a perfectly good draft. The failure branch is
/// where the diagnostic is read, and only there.
///
/// The step names its repository rather than inheriting one. The push step
/// rewrote `origin` to embed the installation token, so a `gh` that resolved
/// the repository from the remote would take the Release the whole publication
/// protocol keys on from a URL another step mutated.
pub(super) const RELEASE_AUTHORITY_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
environment: @ENVIRONMENT@
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the repository without persisted credentials
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download the release candidate
    uses: @DOWNLOAD@
    with:
      name: @JOB@candidate
      path: ${{ runner.temp }}/@JOB@candidate
  - id: @JOB@handoff
    name: Verify the release candidate handoff
    uses: @VERIFY_HANDOFF_ACTION@
    with:
      handoff: ${{ runner.temp }}/@JOB@candidate
      intentional-version: @VERSION@
  - id: @JOB@token
    name: Mint a short-lived repository token
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
  - name: Publish the release commit and the global release tag
    env:
      @ENVVAR@SOURCE_SHA: ${{ steps.@JOB@handoff.outputs.source-sha }}
      @ENVVAR@RELEASE_SHA: ${{ steps.@JOB@handoff.outputs.release-sha }}
      @ENVVAR@GLOBAL_TAG: ${{ steps.@JOB@handoff.outputs.global-tag }}
      @ENVVAR@DEFAULT_BRANCH: ${{ github.event.repository.default_branch }}
      GITHUB_TOKEN: ${{ steps.@JOB@token.outputs.token }}
    run: |
      set -euo pipefail
      git remote set-url origin \
        "https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git"
      observed="$(git ls-remote origin "refs/heads/${@ENVVAR@DEFAULT_BRANCH}" | cut -f1)"
      test "${observed}" = "${@ENVVAR@SOURCE_SHA}"
      git push --atomic origin \
        "${@ENVVAR@RELEASE_SHA}:refs/heads/${@ENVVAR@DEFAULT_BRANCH}" \
        "refs/tags/${@ENVVAR@GLOBAL_TAG}"
  - name: Create the draft GitHub Release for the published tag
    env:
      GH_TOKEN: ${{ steps.@JOB@token.outputs.token }}
      GH_REPO: ${{ github.repository }}
      @ENVVAR@GLOBAL_TAG: ${{ steps.@JOB@handoff.outputs.global-tag }}
    run: |
      set -euo pipefail
      viewed="$(gh release view "${@ENVVAR@GLOBAL_TAG}" \
        --json isDraft --jq '.isDraft' 2>"${RUNNER_TEMP}/@JOB@release-view-error")" \
        && resolved=0 || resolved=$?
      if test "${resolved}" -eq 0; then
        if test "${viewed}" != "true"; then
          printf 'the Release for %s is no longer a draft, so this release has already closed; continuing would place assets on an immutable Release\n' \
            "${@ENVVAR@GLOBAL_TAG}" >&2
          exit 1
        fi
      else
        if ! grep -qi 'release not found' "${RUNNER_TEMP}/@JOB@release-view-error"; then
          printf 'the draft Release could not be resolved: %s\n' \
            "$(cat "${RUNNER_TEMP}/@JOB@release-view-error")" >&2
          exit 1
        fi
        gh release create "${@ENVVAR@GLOBAL_TAG}" --draft --verify-tag \
          --title "${@ENVVAR@GLOBAL_TAG}" --notes ''
      fi
"#;

/// The job that proves the release identity every later job binds itself to.
///
/// The four identities it proves are projected as job outputs because the
/// managed upload job writes them into a draft-asset handoff, and a downstream
/// publisher proves that document against its own checkout before it trusts a
/// single asset. Routing them through the graph is what makes them the verified
/// values: `github.ref_name` and a configured tag template are ambient values
/// that agree with the verified identity right up until they do not, and a
/// handoff carrying one of those would be refused on a release runner by the
/// command that compares them.
pub(super) const PUBLISH_VERIFY_JOB: &str = r#"
runs-on: ubuntu-latest
permissions:
  contents: read
outputs:
  global-tag: ${{ steps.@JOB@verified.outputs.global-tag }}
  global-tag-object: ${{ steps.@JOB@verified.outputs.global-tag-object }}
  source-sha: ${{ steps.@JOB@verified.outputs.source-sha }}
  release-sha: ${{ steps.@JOB@verified.outputs.release-sha }}
  plan-digest: ${{ steps.@JOB@verified.outputs.plan-digest }}
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - id: @JOB@verified
    name: Verify the global release tag
    uses: @VERIFY_RELEASE_TAG_ACTION@
    with:
      intentional-version: @VERSION@
"#;

/// Build job for one distinct publishable subject.
///
/// The bytes and the built-subject document travel together in one artifact:
/// the tag job reads the document and the publisher jobs promote the bytes, and
/// a transport that separated them would let a publisher receive bytes no
/// phase tag ever sealed.
///
/// The version and digest the recording step derived are projected as job
/// outputs because a publisher recipe writes both into its observation and
/// assembly compares them against what the phase tag sealed. Routing them
/// through the graph is what makes them the build's values: a recipe that read
/// a version out of its own package manifest could publish under a version the
/// release plan never assigned and still agree with itself.
pub(super) const PUBLISH_BUILD_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
permissions:
  contents: read
outputs:
  version: ${{ steps.@JOB@record.outputs.version }}
  digest: ${{ steps.@JOB@record.outputs.digest }}
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
@TOOLCHAIN_STEPS@  - name: @BUILD_NAME@
    working-directory: @WORKING_DIRECTORY@
    env:
      @ENVVAR@SUBJECT: ${{ runner.temp }}/@JOB@subject/@SLUG@/bytes
@BUILD_ENV@    run: |
      set -euo pipefail
      mkdir -p "${@ENVVAR@SUBJECT}"
@BUILD_COMMAND@
  - id: @JOB@record
    name: @RECORD_NAME@
    uses: @BUILT_SUBJECT_ACTION@
    with:
      release-unit: @RELEASE_UNIT@
      identity: @SUBJECT_IDENTITY@
      subject: ${{ runner.temp }}/@JOB@subject/@SLUG@/bytes
      output: ${{ runner.temp }}/@JOB@subject/@SLUG@/built-subject.yml
      intentional-version: @VERSION@
  - name: @UPLOAD_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@subject-@SLUG@
      path: ${{ runner.temp }}/@JOB@subject/@SLUG@
      retention-days: 1
  - name: @DOCUMENT_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@subjectdoc-@SLUG@
      path: ${{ runner.temp }}/@JOB@subject/@SLUG@/built-subject.yml
      retention-days: 1
"#;

/// Managed job that seals one executor phase and publishes its tags.
///
/// Tag creation is local and pushing is repository-owned: the portable command
/// never holds a credential and never writes to the repository, and the push
/// step mints the short-lived installation token that is the sole Git
/// repository-write authority.
///
/// The refs pushed are read from the repository rather than named by the
/// derivation, and that is the contract rather than a convenience. This job
/// checks out with `fetch-tags: true` and creates tags in exactly one step, so
/// the annotated tags pointing at the released commit are the ones this phase
/// sealed plus the already-published global tag, whose re-push is a no-op. That
/// set is also what makes the job idempotent: a rerun after a partial failure
/// finds some tags already present, plans none of them, and still pushes the
/// complete set, where a list of newly created refs would be empty and push
/// nothing. Naming the refs from the seal step's output would be structural but
/// would trade that recovery away, so any change to it has to keep the rerun
/// path pushing what the release already carries.
///
/// The sealed evidence is uploaded as its own artifact because assembly reads
/// documents, not tag messages; a phase whose evidence stayed inside Git would
/// be invisible to the command that has to compare it.
pub(super) const PUBLISH_PHASE_TAG_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
environment: @ENVIRONMENT@
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download the staged @PHASE@ evidence
    uses: @DOWNLOAD@
    with:
      pattern: @JOB@@STAGED@-*
      path: ${{ runner.temp }}/@JOB@staged
  - name: @SEAL_NAME@
    uses: @TAG_PHASE_ACTION@
    with:
      phase: @PHASE@
      evidence: ${{ runner.temp }}/@JOB@staged
      sealed-output: ${{ runner.temp }}/@JOB@phase/@PHASE@
      intentional-version: @VERSION@
  - id: @JOB@token
    name: Mint a short-lived repository token
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
  - name: @PUSH_NAME@
    env:
      GITHUB_TOKEN: ${{ steps.@JOB@token.outputs.token }}
    run: |
      set -euo pipefail
      git remote set-url origin \
        "https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git"
      mapfile -t refs < <(git tag --points-at HEAD --format='refs/tags/%(refname:strip=2)')
      test "${#refs[@]}" -gt 0
      git push --atomic origin "${refs[@]}"
  - name: @UPLOAD_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@phase-@PHASE@
      path: ${{ runner.temp }}/@JOB@phase/@PHASE@
      retention-days: 1
"#;

pub(super) const PUBLISH_PUBLISHER_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
permissions:
@PERMISSIONS@
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: @SUBJECT_NAME@
    uses: @DOWNLOAD@
    with:
      name: @JOB@subject-@SUBJECT_SLUG@
      path: ${{ runner.temp }}/@JOB@subject
@HANDOFF_STEP@@RECIPE_STEPS@@VERIFY_STEPS@"#;

/// Retrieval job whose workflow token can read GitHub Packages but cannot publish.
pub(super) const PUBLISH_RETRIEVAL_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
permissions:
  contents: read
  packages: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: @SUBJECT_NAME@
    uses: @DOWNLOAD@
    with:
      name: @JOB@subject-@SUBJECT_SLUG@
      path: ${{ runner.temp }}/@JOB@subject
@HANDOFF_STEP@@RETRIEVAL_STEPS@@VERIFY_STEPS@"#;

/// Verify one observation and upload the resulting publisher-evidence fragment.
pub(super) const PUBLISH_VERIFY_STEPS: &str = r#"  - name: @VERIFY_NAME@
    uses: @VERIFY_PUBLICATION_ACTION@
    with:
      release-unit: @RELEASE_UNIT@
      package: @PACKAGE@
      publisher: @PUBLISHER@
      target: @TARGET@
      observation: @OBSERVATION@
      output: @OUTPUT@
      draft-handoff: @HANDOFF@
      intentional-version: @VERSION@
  - name: @FRAGMENT_NAME@
    uses: @UPLOAD@
    with:
      name: @JOB@evidence-@SLUG@
      path: ${{ runner.temp }}/@JOB@evidence/@SLUG@.yml
      retention-days: 1
"#;

/// Managed job placing every GitHub-hosted deliverable on the draft Release.
///
/// Writing to a Release is `contents: write`, the same permission that pushes
/// commits and moves tags, so the authority stays in a repository-local job
/// inside the protected environment and no publisher job receives it. That
/// makes this an unconditional barrier between every build job and every
/// publisher job: the slowest build gates the first publication, which is the
/// price of one job holding repository write authority instead of one per
/// subject.
///
/// The draft is resolved once, before any asset is written. A Release that is
/// no longer a draft has already closed and a Release carrying another tag is
/// not this release's, and both are refused there rather than discovered
/// halfway through an upload. The resolved identifier is written to a file
/// because a later step needs the same Release and a step cannot read another
/// step's shell variables.
///
/// Uploading is `--clobber`, so a rerun after a partial failure replaces what it
/// already placed and succeeds. `immutable-github-release` states that a failure
/// before closure leaves a resumable draft, and an upload that refused a
/// deliverable already present would make that draft resumable only by hand.
pub(super) const PUBLISH_UPLOAD_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
environment: @ENVIRONMENT@
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download every built subject
    uses: @DOWNLOAD@
    with:
      pattern: @JOB@subject-*
      path: ${{ runner.temp }}/@JOB@subject
  - id: @JOB@token
    name: Mint a short-lived Release token
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
  - name: Resolve the draft Release for the released tag
    env:
      GH_TOKEN: ${{ steps.@JOB@token.outputs.token }}
      GH_REPO: ${{ github.repository }}
      @ENVVAR@GLOBAL_TAG: ${{ needs.@VERIFY@.outputs.global-tag }}
      @ENVVAR@RELEASE_ID: ${{ runner.temp }}/@JOB@draft-release
    run: |
      set -euo pipefail
      viewed="$(gh release view "${@ENVVAR@GLOBAL_TAG}" \
        --json isDraft,tagName,databaseId --jq '[.isDraft, .tagName, .databaseId] | @tsv' \
        2>"${RUNNER_TEMP}/@JOB@release-view-error")" || {
        printf 'the draft Release for %s could not be resolved: %s\n' \
          "${@ENVVAR@GLOBAL_TAG}" "$(cat "${RUNNER_TEMP}/@JOB@release-view-error")" >&2
        exit 1
      }
      IFS=$'\t' read -r drafted named identified <<<"${viewed}"
      if test "${drafted}" != "true"; then
        printf 'the Release for %s is no longer a draft; deliverables are placed before closure, never onto a published Release\n' \
          "${@ENVVAR@GLOBAL_TAG}" >&2
        exit 1
      fi
      if test "${named}" != "${@ENVVAR@GLOBAL_TAG}"; then
        printf 'the Release resolved for %s carries tag %s; the deliverables of this release belong to the draft of its own global release tag\n' \
          "${@ENVVAR@GLOBAL_TAG}" "${named}" >&2
        exit 1
      fi
      printf '%s\n' "${identified}" > "${@ENVVAR@RELEASE_ID}"
@DELIVERABLE_STEPS@@HANDOFF_STEPS@"#;

/// One subject's deliverables reaching the draft Release.
///
/// The bytes are the ones the build job produced and transported. Nothing here
/// runs a packager: a second packaging produces a subject whose digest cannot
/// equal the one the release sealed, which is the whole premise of the
/// build-once graph.
///
/// A Release asset name is flat, so two subjects that produce the same basename
/// would silently replace one another and the handoff of the loser would
/// inventory an identifier holding the winner's bytes. The ledger is what makes
/// that a refusal naming both, and it spans subjects because the collision does.
pub(super) const PUBLISH_UPLOAD_STEP: &str = r#"  - name: @DELIVERABLE_NAME@
    env:
      GH_TOKEN: ${{ steps.@JOB@token.outputs.token }}
      GH_REPO: ${{ github.repository }}
      @ENVVAR@GLOBAL_TAG: ${{ needs.@VERIFY@.outputs.global-tag }}
      @ENVVAR@SUBJECT_IDENTITY: @SUBJECT_IDENTITY@
      @ENVVAR@SUBJECT: ${{ runner.temp }}/@JOB@subject/@JOB@subject-@SLUG@/bytes
      @ENVVAR@LEDGER: ${{ runner.temp }}/@JOB@uploaded
    run: |
      set -euo pipefail
      @ENVVAR@DELIVERABLES=()
      while IFS= read -r -d '' @ENVVAR@DELIVERABLE; do
        @ENVVAR@DELIVERABLES+=("${@ENVVAR@DELIVERABLE}")
      done < <(find "${@ENVVAR@SUBJECT}" \
        -maxdepth 1 -type f @DELIVERABLE_FIND@ -print0 | sort -z)
      if test "${#@ENVVAR@DELIVERABLES[@]}" -eq 0; then
        printf 'the %s build produced no GitHub-hosted deliverable under %s, so this publication has nothing its consumers could resolve\n' \
          "${@ENVVAR@SUBJECT_IDENTITY}" "${@ENVVAR@SUBJECT}" >&2
        exit 1
      fi
      touch "${@ENVVAR@LEDGER}"
      for @ENVVAR@DELIVERABLE in "${@ENVVAR@DELIVERABLES[@]}"; do
        @ENVVAR@ASSET="$(basename "${@ENVVAR@DELIVERABLE}")"
        case "${@ENVVAR@ASSET}" in
          -*|.*|*[!A-Za-z0-9._+-]*)
            printf 'the %s build produced Release asset name %s, which this release cannot carry: an asset name is written into a quoted handoff scalar and passed as its own argument, so it is held to letters, digits, dots, underscores, plus signs and inner hyphens\n' \
              "${@ENVVAR@SUBJECT_IDENTITY}" "${@ENVVAR@ASSET}" >&2
            exit 1
            ;;
        esac
        if grep -qxF "${@ENVVAR@ASSET}" "${@ENVVAR@LEDGER}"; then
          printf 'another subject of this release already places Release asset %s; a Release asset name is flat, so one deliverable would replace the other\n' \
            "${@ENVVAR@ASSET}" >&2
          exit 1
        fi
        printf '%s\n' "${@ENVVAR@ASSET}" >> "${@ENVVAR@LEDGER}"
      done
      gh release upload "${@ENVVAR@GLOBAL_TAG}" "${@ENVVAR@DELIVERABLES[@]}" --clobber
"#;

/// One draft-dependent publication's asset handoff, written from the live draft.
///
/// The inventory is read back from the Release rather than predicted, because
/// the stable asset identifier the consumer resolves is GitHub's and exists only
/// once the asset does. The digest beside it is the one the release sealed: it
/// is taken from the local bytes this job uploaded, so the consumer proving its
/// download against it proves the round trip rather than agreeing with whatever
/// the Release now holds.
///
/// The document is written whole or not at all. A deliverable the draft does not
/// carry is a refusal rather than an omitted entry, because a handoff that
/// inventories a subset is indistinguishable at the consumer from one whose
/// publication legitimately consumes fewer assets.
pub(super) const PUBLISH_HANDOFF_STEP: &str = r#"  - name: @HANDOFF_NAME@
    env:
      GH_TOKEN: ${{ steps.@JOB@token.outputs.token }}
      GH_REPO: ${{ github.repository }}
      @ENVVAR@REPOSITORY: ${{ github.repository }}
      @ENVVAR@GLOBAL_TAG: ${{ needs.@VERIFY@.outputs.global-tag }}
      @ENVVAR@SOURCE_COMMIT: ${{ needs.@VERIFY@.outputs.source-sha }}
      @ENVVAR@RELEASE_COMMIT: ${{ needs.@VERIFY@.outputs.release-sha }}
      @ENVVAR@PLAN_DIGEST: ${{ needs.@VERIFY@.outputs.plan-digest }}
      @ENVVAR@RELEASE_ID: ${{ runner.temp }}/@JOB@draft-release
      @ENVVAR@RELEASE_UNIT: @RELEASE_UNIT@
      @ENVVAR@PACKAGE: @PACKAGE@
      @ENVVAR@PUBLISHER: @PUBLISHER@
      @ENVVAR@TARGET: @TARGET@
      @ENVVAR@PUBLICATION: @PUBLICATION@
      @ENVVAR@SUBJECT: ${{ runner.temp }}/@JOB@subject/@JOB@subject-@SUBJECT_SLUG@/bytes
      @ENVVAR@HANDOFF: @HANDOFF@
    run: |
      set -euo pipefail
      @ENVVAR@RELEASE="$(cat "${@ENVVAR@RELEASE_ID}")"
      @ENVVAR@CONSUMED=()
      while IFS= read -r -d '' @ENVVAR@DELIVERABLE; do
        @ENVVAR@CONSUMED+=("${@ENVVAR@DELIVERABLE}")
      done < <(find "${@ENVVAR@SUBJECT}" \
        -maxdepth 1 -type f @DELIVERABLE_FIND@ @CONSUMED_FIND@ -print0 | sort -z)
      if test "${#@ENVVAR@CONSUMED[@]}" -eq 0; then
        printf 'the %s publication consumes a GitHub Release asset, but its subject produced none under %s\n' \
          "${@ENVVAR@PUBLICATION}" "${@ENVVAR@SUBJECT}" >&2
        exit 1
      fi
      gh api --paginate "repos/${@ENVVAR@REPOSITORY}/releases/${@ENVVAR@RELEASE}/assets" \
        --jq '.[] | [.name, .id, .size, .content_type] | @tsv' \
        > "${RUNNER_TEMP}/@JOB@inventory"
      mkdir -p "$(dirname "${@ENVVAR@HANDOFF}")"
      {
        # $schema is a literal YAML key, not a shell expansion.
        # shellcheck disable=SC2016
        printf '$schema: %s\n' "@HANDOFF_SCHEMA@"
        printf 'contract: %s\n' "@HANDOFF_CONTRACT@"
        printf 'repository: "%s"\n' "${@ENVVAR@REPOSITORY}"
        printf 'release-id: %s\n' "${@ENVVAR@RELEASE}"
        printf 'global-tag: "%s"\n' "${@ENVVAR@GLOBAL_TAG}"
        printf 'source-commit: "%s"\n' "${@ENVVAR@SOURCE_COMMIT}"
        printf 'release-commit: "%s"\n' "${@ENVVAR@RELEASE_COMMIT}"
        printf 'plan-digest: "%s"\n' "${@ENVVAR@PLAN_DIGEST}"
        printf 'release-unit: "%s"\n' "${@ENVVAR@RELEASE_UNIT}"
        printf 'package: "%s"\n' "${@ENVVAR@PACKAGE}"
        printf 'publisher: %s\n' "${@ENVVAR@PUBLISHER}"
        printf 'target: "%s"\n' "${@ENVVAR@TARGET}"
        printf 'assets:\n'
        for @ENVVAR@ASSET_PATH in "${@ENVVAR@CONSUMED[@]}"; do
          @ENVVAR@ASSET="$(basename "${@ENVVAR@ASSET_PATH}")"
          @ENVVAR@ENTRY="$(awk -F'\t' -v name="${@ENVVAR@ASSET}" \
            '$1 == name { print; exit }' "${RUNNER_TEMP}/@JOB@inventory")"
          if test -z "${@ENVVAR@ENTRY}"; then
            printf 'deliverable %s is not an asset of draft Release %s, so the %s publication could not retrieve it\n' \
              "${@ENVVAR@ASSET}" "${@ENVVAR@RELEASE}" "${@ENVVAR@PUBLICATION}" >&2
            exit 1
          fi
          IFS=$'\t' read -r _ @ENVVAR@ID @ENVVAR@SIZE @ENVVAR@MEDIA <<<"${@ENVVAR@ENTRY}"
          case "${@ENVVAR@MEDIA}" in
            ''|*[!A-Za-z0-9._+/-]*)
              printf 'draft Release asset %s declares media type %s, which this handoff cannot carry as a quoted scalar\n' \
                "${@ENVVAR@ASSET}" "${@ENVVAR@MEDIA}" >&2
              exit 1
              ;;
          esac
          printf -- '- id: %s\n' "${@ENVVAR@ID}"
          printf '  name: "%s"\n' "${@ENVVAR@ASSET}"
          printf '  size: %s\n' "${@ENVVAR@SIZE}"
          printf '  media-type: "%s"\n' "${@ENVVAR@MEDIA}"
          printf '  sha256: "sha256:%s"\n' \
            "$(sha256sum "${@ENVVAR@ASSET_PATH}" | cut -d' ' -f1)"
        done
      } > "${@ENVVAR@HANDOFF}"
  - name: @HANDOFF_UPLOAD_NAME@
    uses: @UPLOAD@
    with:
      name: @HANDOFF_ARTIFACT@
      path: @HANDOFF_DIRECTORY@
      retention-days: 1
"#;

pub(super) const PUBLISH_ASSEMBLE_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download every evidence fragment
    uses: @DOWNLOAD@
    with:
      pattern: @JOB@evidence-*
      path: ${{ runner.temp }}/@JOB@fragments
  - name: Download every sealed phase document
    uses: @DOWNLOAD@
    with:
      pattern: @JOB@phase-*
      path: ${{ runner.temp }}/@JOB@fragments
  - name: Assemble the release evidence
    uses: @ASSEMBLE_ACTION@
    with:
      input: ${{ runner.temp }}/@JOB@fragments
      output: ${{ runner.temp }}/@JOB@release
      intentional-version: @VERSION@
  - name: Upload the assembled release evidence
    uses: @UPLOAD@
    with:
      name: @JOB@release-evidence
      path: ${{ runner.temp }}/@JOB@release
      retention-days: 1
"#;

pub(super) const PUBLISH_CLOSE_JOB: &str = r#"
needs:
@NEEDS@
runs-on: ubuntu-latest
environment: @ENVIRONMENT@
permissions:
  contents: read
env:
  @ENVVAR@WORKFLOW_CONTRACT: @CONTRACT@
steps:
  - id: @SENTINEL@
    name: Check out the released commit
    uses: @CHECKOUT@
    with:
      fetch-depth: 0
      fetch-tags: true
      persist-credentials: false
  - name: Download the assembled release evidence
    uses: @DOWNLOAD@
    with:
      name: @JOB@release-evidence
      path: ${{ runner.temp }}/@JOB@release
  - id: @JOB@token
    name: Mint a short-lived Release token
    uses: @APP_TOKEN@
    with:
      app-id: ${{ vars.@ENVVAR@GITHUB_APP_ID }}
      private-key: ${{ secrets.@ENVVAR@GITHUB_APP_PRIVATE_KEY }}
  - name: Publish the immutable GitHub Release
    env:
      GH_TOKEN: ${{ steps.@JOB@token.outputs.token }}
      @ENVVAR@GLOBAL_TAG: ${{ github.ref_name }}
      @ENVVAR@RELEASE: ${{ runner.temp }}/@JOB@release
    run: |
      set -euo pipefail
      drafted="$(gh release view "${@ENVVAR@GLOBAL_TAG}" --json isDraft --jq '.isDraft')"
      if test "${drafted}" != "true"; then
        printf 'the Release for %s is already published, so there is nothing left to close and its assets are frozen\n' \
          "${@ENVVAR@GLOBAL_TAG}" >&2
        exit 1
      fi
      named="$(gh release view "${@ENVVAR@GLOBAL_TAG}" --json tagName --jq '.tagName')"
      if test "${named}" != "${@ENVVAR@GLOBAL_TAG}"; then
        printf 'the Release resolved for %s carries tag %s; closure publishes the draft of its own global release tag\n' \
          "${@ENVVAR@GLOBAL_TAG}" "${named}" >&2
        exit 1
      fi
      targeted="$(gh api "repos/${GITHUB_REPOSITORY}/git/ref/tags/${@ENVVAR@GLOBAL_TAG}" \
        --jq '.object.sha' | xargs -I {} gh api "repos/${GITHUB_REPOSITORY}/git/tags/{}" \
        --jq '.object.sha')"
      if test "${targeted}" != "${GITHUB_SHA}"; then
        printf 'the annotated tag %s targets %s, not the released commit %s this run is closing\n' \
          "${@ENVVAR@GLOBAL_TAG}" "${targeted}" "${GITHUB_SHA}" >&2
        exit 1
      fi
      gh release upload "${@ENVVAR@GLOBAL_TAG}" \
        "${@ENVVAR@RELEASE}"/* --clobber
      for asset in "${@ENVVAR@RELEASE}"/*; do
        name="$(basename "${asset}")"
        gh release download "${@ENVVAR@GLOBAL_TAG}" --pattern "${name}" \
          --output - > "${RUNNER_TEMP}/@JOB@closure-asset"
        if ! printf '%s  %s\n' "$(sha256sum < "${asset}" | cut -d' ' -f1)" \
          "${RUNNER_TEMP}/@JOB@closure-asset" | sha256sum --check --status; then
          printf 'the Release asset %s reads back as bytes this release did not assemble; the draft is left unpublished\n' \
            "${name}" >&2
          exit 1
        fi
      done
      gh release edit "${@ENVVAR@GLOBAL_TAG}" --draft=false
"#;
