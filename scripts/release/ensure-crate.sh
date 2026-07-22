#!/usr/bin/env bash
# ---
# relationships:
#   publishes: intent-driven-polyglot-release
# ---

set -euo pipefail

crate="${1:?crate name is required}"
version="${2:?version is required}"
registry_url="https://crates.io/api/v1/crates/${crate}/${version}"

cargo publish -p "$crate" --locked --dry-run
cargo package -p "$crate" --locked

archive="target/package/${crate}-${version}.crate"
test -f "$archive"
local_checksum="$(sha256sum "$archive" | cut -d' ' -f1)"

registry_response="$(mktemp)"
trap 'rm -f "$registry_response"' EXIT

lookup() {
  curl --silent --show-error \
    --header 'User-Agent: intentional-release-workflow' \
    --output "$registry_response" \
    --write-out '%{http_code}' \
    "$registry_url"
}

status="$(lookup)"
case "$status" in
  200)
    published_checksum="$(jq -r '.version.checksum // empty' "$registry_response")"
    if [[ "$published_checksum" != "$local_checksum" ]]; then
      echo "$crate $version already exists with different package bytes." >&2
      exit 1
    fi
    echo "$crate $version already matches crates.io; skipping publication."
    exit 0
    ;;
  404) ;;
  *)
    echo "crates.io lookup for $crate $version failed with HTTP $status." >&2
    exit 1
    ;;
esac

cargo publish -p "$crate" --locked

for _attempt in {1..18}; do
  sleep 10
  status="$(lookup)"
  if [[ "$status" == "200" ]]; then
    published_checksum="$(jq -r '.version.checksum // empty' "$registry_response")"
    if [[ "$published_checksum" != "$local_checksum" ]]; then
      echo "$crate $version appeared with different package bytes." >&2
      exit 1
    fi
    echo "$crate $version is available on crates.io with matching bytes."
    exit 0
  fi
  if [[ "$status" != "404" ]]; then
    echo "crates.io verification for $crate $version failed with HTTP $status." >&2
    exit 1
  fi
done

echo "$crate $version was published but did not become observable in time." >&2
exit 1
