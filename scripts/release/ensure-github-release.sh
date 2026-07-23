#!/usr/bin/env bash
# ---
# relationships:
#   publishes: intent-driven-polyglot-release
# ---

set -euo pipefail

tag="${1:?tag is required}"
artifact_directory="${2:?artifact directory is required}"
notes_file="${3:?release notes file is required}"

assets=(
  "$artifact_directory/intentional-linux-x86_64.tar.gz"
  "$artifact_directory/intentional-linux-arm64.tar.gz"
  "$artifact_directory/intentional-macos-arm64.tar.gz"
  "$artifact_directory/intentional-windows-x86_64.zip"
  "$artifact_directory/SHA256SUMS"
)

for asset in "${assets[@]}"; do
  if [[ ! -f "$asset" ]]; then
    echo "Missing expected release asset: $asset" >&2
    exit 1
  fi
done

tag_target="$(git rev-list -n 1 "$tag^{commit}")"
head_target="$(git rev-parse HEAD)"
if [[ "$tag_target" != "$head_target" ]]; then
  echo "Tag $tag targets $tag_target, but checkout is $head_target." >&2
  exit 1
fi

if ! gh release view "$tag" >/dev/null 2>&1; then
  gh release create "$tag" "${assets[@]}" \
    --title "$tag" \
    --notes-file "$notes_file" \
    --verify-tag
fi

release_state="$(gh release view "$tag" --json isDraft,isPrerelease,isImmutable \
  --jq '[.isDraft,.isPrerelease,.isImmutable] | @tsv')"
if [[ "$release_state" != $'false\tfalse\ttrue' ]]; then
  echo "GitHub Release $tag must be published, final, and immutable." >&2
  exit 1
fi
gh release verify "$tag" >/dev/null

download_directory="$(mktemp -d)"
trap 'rm -rf "$download_directory"' EXIT

for asset in "${assets[@]}"; do
  name="$(basename "$asset")"
  asset_count="$(gh release view "$tag" --json assets --jq \
    "[.assets[] | select(.name == \"$name\")] | length")"
  case "$asset_count" in
    0)
      echo "Immutable GitHub Release $tag is missing expected asset $name." >&2
      exit 1
      ;;
    1)
      gh release download "$tag" --pattern "$name" --dir "$download_directory"
      local_checksum="$(sha256sum "$asset" | cut -d' ' -f1)"
      remote_checksum="$(sha256sum "$download_directory/$name" | cut -d' ' -f1)"
      if [[ "$local_checksum" != "$remote_checksum" ]]; then
        echo "Existing GitHub Release asset $name has different bytes." >&2
        exit 1
      fi
      gh release verify-asset "$tag" "$download_directory/$name" >/dev/null
      ;;
    *)
      echo "Existing GitHub Release contains duplicate assets named $name." >&2
      exit 1
      ;;
  esac
done

echo "GitHub Release $tag is immutable and contains matching attested assets."
