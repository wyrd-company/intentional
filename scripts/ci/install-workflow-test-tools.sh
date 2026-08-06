#!/usr/bin/env bash
# ---
# relationships:
#   validates: github-release-executor
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Root is resolved from this script at runtime.
# shellcheck disable=SC1091
source "$root/scripts/release/linux-gnu-baseline.env"
for agreement in passthrough volumes; do
  if [[ "$agreement" == passthrough ]]; then
    environment="$GNU_CROSS_PASSTHROUGH_ENVIRONMENT"
  else
    environment="$GNU_CROSS_VOLUME_ENVIRONMENT"
  fi
  cross_environment='["'"${environment// /\", \"}"'"]'
  if ! grep -Fqx "$agreement = $cross_environment" "$root/Cross.toml"; then
    echo "pinned GNU Cross $agreement agreement must be $cross_environment" >&2
    exit 1
  fi
done

destination="${1:?destination directory is required}"
mkdir -p "$destination"
destination="$(cd "$destination" && pwd)"

case "$(uname -m)" in
  x86_64)
    actionlint_arch="amd64"
    actionlint_digest="023070a287cd8cccd71515fedc843f1985bf96c436b7effaecce67290e7e0757"
    jq_arch="amd64"
    jq_digest="5942c9b0934e510ee61eb3e30273f1b3fe2590df93933a93d7c58b81d19c8ff5"
    shellcheck_arch="x86_64"
    shellcheck_digest="6c881ab0698e4e6ea235245f22832860544f17ba386442fe7e9d629f8cbedf87"
    ;;
  aarch64|arm64)
    actionlint_arch="arm64"
    actionlint_digest="401942f9c24ed71e4fe71b76c7d638f66d8633575c4016efd2977ce7c28317d0"
    jq_arch="arm64"
    jq_digest="4dd2d8a0661df0b22f1bb9a1f9830f06b6f3b8f7d91211a1ef5d7c4f06a8b4a5"
    shellcheck_arch="aarch64"
    shellcheck_digest="324a7e89de8fa2aed0d0c28f3dab59cf84c6d74264022c00c22af665ed1a09bb"
    ;;
  *)
    echo "unsupported workflow-test tool architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

actionlint_archive="actionlint_1.7.7_linux_${actionlint_arch}.tar.gz"
curl -fsSL --max-redirs 5 \
  "https://github.com/rhysd/actionlint/releases/download/v1.7.7/$actionlint_archive" \
  -o "$temporary/$actionlint_archive"
printf '%s  %s\n' "$actionlint_digest" "$temporary/$actionlint_archive" | sha256sum --check
tar -xzf "$temporary/$actionlint_archive" -C "$destination" actionlint

jq_asset="jq-linux-${jq_arch}"
curl -fsSL --max-redirs 5 \
  "https://github.com/jqlang/jq/releases/download/jq-1.7.1/$jq_asset" \
  -o "$temporary/$jq_asset"
printf '%s  %s\n' "$jq_digest" "$temporary/$jq_asset" | sha256sum --check
install -m 0755 "$temporary/$jq_asset" "$destination/jq"

shellcheck_archive="shellcheck-v0.10.0.linux.${shellcheck_arch}.tar.xz"
curl -fsSL --max-redirs 5 \
  "https://github.com/koalaman/shellcheck/releases/download/v0.10.0/$shellcheck_archive" \
  -o "$temporary/$shellcheck_archive"
printf '%s  %s\n' "$shellcheck_digest" "$temporary/$shellcheck_archive" | sha256sum --check
tar -xJf "$temporary/$shellcheck_archive" -C "$temporary"
install -m 0755 "$temporary/shellcheck-v0.10.0/shellcheck" "$destination/shellcheck"

cat > "$destination/pinned-gnu-rustc-wrapper" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

for variable in ACTIONLINT JQ SHELLCHECK; do
  value="${!variable:-}"
  if [[ -z "$value" || ! -x "$value" ]]; then
    echo "pinned GNU workflow-tool mount witness: $variable is not executable at ${value:-<unset>}" >&2
    exit 1
  fi
done

if [[ "${CARGO_HOME:-}" == /cargo ]]; then
  git_version="$(git --version)"
  if [[ "$git_version" != "git version $GNU_BUILD_GIT_VERSION" ]]; then
    echo "pinned GNU baseline witness: expected git $GNU_BUILD_GIT_VERSION, found $git_version" >&2
    exit 1
  fi
  if [[ "${BASH_VERSION%%(*}" != "$GNU_BUILD_BASH_VERSION" ]]; then
    echo "pinned GNU baseline witness: expected bash $GNU_BUILD_BASH_VERSION, found $BASH_VERSION" >&2
    exit 1
  fi
fi

exec "$@"
EOF
chmod 0755 "$destination/pinned-gnu-rustc-wrapper"

cat > "$destination/workflow-test-tools.env" <<EOF
ACTIONLINT=$destination/actionlint
JQ=$destination/jq
SHELLCHECK=$destination/shellcheck
RUSTC_WRAPPER=$destination/pinned-gnu-rustc-wrapper
GNU_BUILD_GIT_VERSION=$GNU_BUILD_GIT_VERSION
GNU_BUILD_BASH_VERSION=$GNU_BUILD_BASH_VERSION
GNU_CROSS_TEST_ARGUMENTS=$GNU_CROSS_TEST_ARGUMENTS
EOF

write_shell_assignment() {
  local quoted
  # $value is a jq variable.
  # shellcheck disable=SC2016
  quoted=$("$destination/jq" -Rrn --arg value "$2" '$value | @sh')
  printf '%s=%s\n' "$1" "$quoted"
}

{
  write_shell_assignment ACTIONLINT "$destination/actionlint"
  write_shell_assignment JQ "$destination/jq"
  write_shell_assignment SHELLCHECK "$destination/shellcheck"
  write_shell_assignment RUSTC_WRAPPER "$destination/pinned-gnu-rustc-wrapper"
  write_shell_assignment GNU_BUILD_GIT_VERSION "$GNU_BUILD_GIT_VERSION"
  write_shell_assignment GNU_BUILD_BASH_VERSION "$GNU_BUILD_BASH_VERSION"
  write_shell_assignment GNU_CROSS_TEST_ARGUMENTS "$GNU_CROSS_TEST_ARGUMENTS"
} > "$destination/workflow-test-tools.sh"

"$destination/actionlint" -version
"$destination/jq" --version
"$destination/shellcheck" --version
