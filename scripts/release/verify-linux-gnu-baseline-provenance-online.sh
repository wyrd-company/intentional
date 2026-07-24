#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
#   references: build-linux-gnu-release-on-pinned-cross-baseline
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
baseline_env="$root/scripts/release/linux-gnu-baseline.env"

for tool in curl jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Required online provenance tool is unavailable: $tool" >&2
    exit 1
  fi
done

# shellcheck disable=SC1090
source "$baseline_env"

registry_token() {
  local repository="$1"
  curl -fsSL \
    "https://ghcr.io/token?service=ghcr.io&scope=repository:${repository}:pull" |
    jq -er .token
}

manifest_digest_for_tag() {
  local repository="$1"
  local tag="$2"
  local token
  local headers

  token="$(registry_token "$repository")"
  headers="$(mktemp)"
  curl -fsSL -D "$headers" -o /dev/null \
    -H "Authorization: Bearer $token" \
    -H "Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json" \
    "https://ghcr.io/v2/${repository}/manifests/${tag}"
  sed -n 's/^[Dd]ocker-[Cc]ontent-[Dd]igest: //p' "$headers" | tr -d '\r'
  rm -f "$headers"
}

amd64_linux_manifest_digest() {
  local repository="$1"
  local index_digest="$2"
  local token

  token="$(registry_token "$repository")"
  curl -fsSL \
    -H "Authorization: Bearer $token" \
    -H "Accept: application/vnd.oci.image.index.v1+json" \
    "https://ghcr.io/v2/${repository}/manifests/${index_digest}" |
    jq -er '
      [.manifests[]
        | select(.platform.architecture == "amd64" and .platform.os == "linux")
        | .digest][0]
    '
}

image_config_labels() {
  local repository="$1"
  local manifest_digest="$2"
  local token config_digest

  token="$(registry_token "$repository")"
  config_digest="$(
    curl -fsSL \
      -H "Authorization: Bearer $token" \
      -H "Accept: application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json" \
      "https://ghcr.io/v2/${repository}/manifests/${manifest_digest}" |
      jq -er .config.digest
  )"

  curl -fsSL \
    -H "Authorization: Bearer $token" \
    "https://ghcr.io/v2/${repository}/blobs/${config_digest}" |
    jq -ec '{architecture, os, labels: .config.Labels}'
}

verify_cross_image() {
  local image_ref="$1"
  local repository="${image_ref#ghcr.io/}"
  repository="${repository%%:*}"
  local pinned_index_digest="${image_ref##*@}"
  local tag="$CROSS_VERSION"

  echo "repository=$repository"
  echo "public_tag=$tag"
  echo "pinned_index_digest=$pinned_index_digest"

  local published_index_digest amd64_manifest config_json
  published_index_digest="$(manifest_digest_for_tag "$repository" "$tag")"
  echo "published_index_digest=$published_index_digest"

  if [[ "$published_index_digest" != "$pinned_index_digest" ]]; then
    echo "Public tag ${repository}:${tag} resolves to ${published_index_digest}, expected ${pinned_index_digest}." >&2
    exit 1
  fi

  amd64_manifest="$(amd64_linux_manifest_digest "$repository" "$published_index_digest")"
  echo "amd64_linux_manifest_digest=$amd64_manifest"

  config_json="$(image_config_labels "$repository" "$amd64_manifest")"
  echo "image_config=$config_json"

  jq -e --arg arch "$GNU_IMAGE_HOST_ARCH" --arg revision "$CROSS_OCI_REVISION" \
    --arg version "$CROSS_OCI_VERSION" --arg source "$CROSS_OCI_SOURCE" '
    .architecture == $arch and
    .os == "linux" and
    .labels["org.opencontainers.image.revision"] == $revision and
    .labels["org.opencontainers.image.version"] == $version and
    .labels["org.opencontainers.image.source"] == $source
  ' <<<"$config_json" >/dev/null || {
    echo "Image config labels for ${repository}:${tag} do not match linux-gnu-baseline.env." >&2
    echo "expected architecture=$GNU_IMAGE_HOST_ARCH revision=$CROSS_OCI_REVISION version=$CROSS_OCI_VERSION source=$CROSS_OCI_SOURCE" >&2
    jq . <<<"$config_json" >&2
    exit 1
  }

  echo "online provenance verified for ${repository}:${tag}"
}

verify_cross_image "$X86_64_GNU_CROSS_IMAGE"
verify_cross_image "$AARCH64_GNU_CROSS_IMAGE"

echo "Linux GNU baseline online provenance verified against ghcr.io."
