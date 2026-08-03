#!/usr/bin/env bash
# ---
# relationships:
#   validates: github-release-executor
# ---
#
# Assert that every repository path an action document reaches through
# GITHUB_ACTION_PATH resolves from that action's own directory.
#
# Composite actions cannot share steps, so the release-protocol actions invoke
# shared scripts relative to their own location. Nothing else enforces that
# relationship, so moving either an action or a script must fail this gate
# rather than a release.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

checked=0
failed=0

for action in action.yml actions/*/action.yml; do
  action_directory="$(dirname "$action")"
  # The grep pattern matches a literal action-path reference rather than a
  # shell expansion, so it is written to survive single-quote linting.
  while IFS= read -r reference; do
    relative="${reference#\$GITHUB_ACTION_PATH/}"
    resolved="$action_directory/$relative"
    checked=$((checked + 1))
    if [[ ! -f "$resolved" ]]; then
      echo "$action references $reference, which does not resolve to a file" >&2
      failed=$((failed + 1))
      continue
    fi
    # A script run through an explicit interpreter does not need its own
    # executable bit; one invoked directly does.
    if grep -qE "(^|[^[:alnum:]_])(bash|sh) +\"?[\$]GITHUB_ACTION_PATH/${relative//\//\\/}" "$action"; then
      continue
    fi
    if [[ ! -x "$resolved" ]]; then
      echo "$action references $reference, which is invoked directly but is not executable" >&2
      failed=$((failed + 1))
    fi
  done < <(grep -o -- '[$]GITHUB_ACTION_PATH/[A-Za-z0-9._/-]*' "$action" | sort -u)
done

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

if [[ "$checked" -eq 0 ]]; then
  echo "No action referenced a repository script; the assertion is vacuous." >&2
  exit 1
fi

echo "Action script references resolve from their own action directories ($checked checked)."
