#!/usr/bin/env bash
# ---
# relationships:
#   validates: github-release-executor
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow_directory="${1:-$root/.github/workflows}"
declaration="${2:-$root/github-action-pins.yml}"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
failures=0

if ! command -v yq >/dev/null 2>&1; then
  echo "the workflow pin census requires yq, but it is unavailable" >&2
  exit 1
fi

find "$workflow_directory" -maxdepth 1 -type f \
  \( -name '*.yml' -o -name '*.yaml' \) -print | sort >"$temporary/workflows"
if [[ ! -s "$temporary/workflows" ]]; then
  echo "the workflow pin census found no workflow files" >&2
  exit 1
fi

while IFS= read -r workflow; do
  uses_file="$temporary/$(basename "$workflow").uses"
  if ! yq -r '.jobs[] | (.uses, .steps[]?.uses) | select(. != null)' \
    "$workflow" >"$uses_file" 2>"$temporary/yq.stderr"; then
    echo "the workflow pin census could not extract uses entries from $workflow with yq: $(cat "$temporary/yq.stderr")" >&2
    failures=$((failures + 1))
    continue
  fi
  while IFS= read -r use; do
    if [[ "$use" == ./* ]]; then
      continue
    fi
    action="${use%@*}"
    repository="$(cut -d/ -f1-2 <<<"$action")"
    workflow_commit="${use##*@}"
    if [[ ! "$workflow_commit" =~ ^[0-9a-f]{40}$ ]]; then
      echo "$workflow uses $use instead of a complete commit" >&2
      failures=$((failures + 1))
    elif ! awk -v repository="$repository" -v commit="$workflow_commit" '
      $1 == "repository:" { candidate = $2 }
      $1 == "commit:" && candidate == repository && $2 == commit { found = 1 }
      END { exit !found }
    ' "$declaration"; then
      echo "$workflow uses undeclared Action identity $use" >&2
      failures=$((failures + 1))
    fi
  done <"$uses_file"
done <"$temporary/workflows"

if [[ "$failures" -ne 0 ]]; then
  echo "$failures workflow Action references are not declared immutable identities" >&2
  exit 1
fi

echo "Every repository workflow uses declared immutable Action identities."
