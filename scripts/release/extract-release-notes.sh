#!/usr/bin/env bash
# ---
# relationships:
#   publishes: intent-driven-polyglot-release
# ---

set -euo pipefail

tag="${1:?tag is required}"
changelog="${2:?changelog path is required}"
output="${3:?output path is required}"

if [[ ! -f "$changelog" ]]; then
  echo "Changelog $changelog does not exist." >&2
  exit 1
fi

awk -v heading="## $tag" '
  $0 == heading {
    found = 1
  }
  found && /^## / && $0 != heading {
    exit
  }
  found {
    print
  }
  END {
    if (!found) exit 2
  }
' "$changelog" > "$output" || {
  echo "Changelog does not contain an exact ## $tag release section." >&2
  exit 1
}

if [[ ! -s "$output" ]] || [[ "$(head -n 1 "$output")" != "## $tag" ]]; then
  echo "Release notes for $tag are empty or incorrectly bound." >&2
  exit 1
fi
