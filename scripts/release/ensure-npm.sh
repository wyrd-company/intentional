#!/usr/bin/env bash
# ---
# relationships:
#   publishes: intent-driven-polyglot-release
# ---

set -euo pipefail

package_root="$(cd "${1:-npm}" && pwd)"
version="${2:?version is required}"
package_name="@wyrd-company/intentional"
encoded_name="%40wyrd-company%2Fintentional"
registry_url="https://registry.npmjs.org/${encoded_name}"

package_name_from_source="$(jq -r '.name' "$package_root/package.json")"
package_version_from_source="$(jq -r '.version' "$package_root/package.json")"
if [[ "$package_name_from_source" != "$package_name" || "$package_version_from_source" != "$version" ]]; then
  echo "npm source identity does not match $package_name@$version." >&2
  exit 1
fi

pack_directory="$(mktemp -d)"
registry_response="$(mktemp)"
trap 'rm -rf "$pack_directory"; rm -f "$registry_response"' EXIT

npm pack "$package_root" \
  --pack-destination "$pack_directory" \
  --ignore-scripts \
  --json \
  --userconfig=/dev/null \
  >/dev/null
archive="$(find "$pack_directory" -maxdepth 1 -type f -name '*.tgz' -print -quit)"
test -n "$archive"
local_integrity="sha512-$(openssl dgst -sha512 -binary "$archive" | base64 -w0)"

lookup() {
  curl --silent --show-error \
    --header 'User-Agent: intentional-release-workflow' \
    --output "$registry_response" \
    --write-out '%{http_code}' \
    "$registry_url"
}

matching_integrity() {
  jq -r --arg version "$version" '.versions[$version].dist.integrity // empty' "$registry_response"
}

status="$(lookup)"
case "$status" in
  200)
    published_integrity="$(matching_integrity)"
    if [[ -n "$published_integrity" ]]; then
      if [[ "$published_integrity" != "$local_integrity" ]]; then
        echo "$package_name@$version already exists with different package bytes." >&2
        exit 1
      fi
      echo "$package_name@$version already matches npmjs; skipping publication."
      exit 0
    fi
    ;;
  404) ;;
  *)
    echo "npmjs lookup for $package_name@$version failed with HTTP $status." >&2
    exit 1
    ;;
esac

npm publish "$package_root" --access public --provenance

for _attempt in {1..18}; do
  sleep 10
  status="$(lookup)"
  if [[ "$status" == "200" ]]; then
    published_integrity="$(matching_integrity)"
    if [[ -n "$published_integrity" ]]; then
      if [[ "$published_integrity" != "$local_integrity" ]]; then
        echo "$package_name@$version appeared with different package bytes." >&2
        exit 1
      fi
      echo "$package_name@$version is available on npmjs with matching bytes."
      exit 0
    fi
  elif [[ "$status" != "404" ]]; then
    echo "npmjs verification for $package_name@$version failed with HTTP $status." >&2
    exit 1
  fi
done

echo "$package_name@$version was published but did not become observable in time." >&2
exit 1
