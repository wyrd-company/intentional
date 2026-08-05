#!/usr/bin/env bash
# ---
# relationships:
#   validates: github-release-executor
# ---

set -euo pipefail

destination="${1:?destination directory is required}"
mkdir -p "$destination"

case "$(uname -m)" in
  x86_64)
    actionlint_arch="amd64"
    actionlint_digest="023070a287cd8cccd71515fedc843f1985bf96c436b7effaecce67290e7e0757"
    shellcheck_arch="x86_64"
    shellcheck_digest="6c881ab0698e4e6ea235245f22832860544f17ba386442fe7e9d629f8cbedf87"
    ;;
  aarch64|arm64)
    actionlint_arch="arm64"
    actionlint_digest="401942f9c24ed71e4fe71b76c7d638f66d8633575c4016efd2977ce7c28317d0"
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
curl -fsSL \
  "https://github.com/rhysd/actionlint/releases/download/v1.7.7/$actionlint_archive" \
  -o "$temporary/$actionlint_archive"
printf '%s  %s\n' "$actionlint_digest" "$temporary/$actionlint_archive" | sha256sum --check
tar -xzf "$temporary/$actionlint_archive" -C "$destination" actionlint

shellcheck_archive="shellcheck-v0.10.0.linux.${shellcheck_arch}.tar.xz"
curl -fsSL \
  "https://github.com/koalaman/shellcheck/releases/download/v0.10.0/$shellcheck_archive" \
  -o "$temporary/$shellcheck_archive"
printf '%s  %s\n' "$shellcheck_digest" "$temporary/$shellcheck_archive" | sha256sum --check
tar -xJf "$temporary/$shellcheck_archive" -C "$temporary"
install -m 0755 "$temporary/shellcheck-v0.10.0/shellcheck" "$destination/shellcheck"

"$destination/actionlint" -version
"$destination/shellcheck" --version
