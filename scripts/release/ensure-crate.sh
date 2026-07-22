#!/usr/bin/env bash
# ---
# relationships:
#   publishes: intent-driven-polyglot-release
# ---

set -euo pipefail

crate="${1:?crate name is required}"
version="${2:?version is required}"
case "${#crate}" in
  1) index_path="1/$crate" ;;
  2) index_path="2/$crate" ;;
  3) index_path="3/${crate:0:1}/$crate" ;;
  *) index_path="${crate:0:2}/${crate:2:2}/$crate" ;;
esac
registry_url="https://index.crates.io/$index_path"

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

published_checksum() {
  jq -r --arg version "$version" \
    'select(.vers == $version) | .cksum' \
    "$registry_response" | tail -n 1
}

status="$(lookup)"
case "$status" in
  200)
    observed_checksum="$(published_checksum)"
    if [[ -z "$observed_checksum" ]]; then
      status=404
    elif [[ "$observed_checksum" != "$local_checksum" ]]; then
      echo "$crate $version already exists with different package bytes." >&2
      exit 1
    else
      echo "$crate $version already matches the crates.io sparse index; skipping publication."
      exit 0
    fi
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
    observed_checksum="$(published_checksum)"
    if [[ -z "$observed_checksum" ]]; then
      continue
    fi
    if [[ "$observed_checksum" != "$local_checksum" ]]; then
      echo "$crate $version appeared with different package bytes." >&2
      exit 1
    fi
    echo "$crate $version is available in the crates.io sparse index with matching bytes."
    exit 0
  fi
  if [[ "$status" != "404" ]]; then
    echo "crates.io verification for $crate $version failed with HTTP $status." >&2
    exit 1
  fi
done

echo "$crate $version was published but did not become observable in time." >&2
exit 1
