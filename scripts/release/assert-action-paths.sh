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

#
# The tree to check defaults to this repository. A caller may name another so
# the gate itself can be held to negative cases, because a gate only ever run
# against compliant files cannot distinguish "the rule holds" from "the rule is
# never evaluated".

set -euo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
cd "$root"

checked=0
failed=0

for action in action.yml actions/*/action.yml; do
  # An unmatched glob and a tree without a root action document both arrive as
  # a path that is not a file. Skipping them here keeps the vacuity check below
  # the single place that decides an empty sweep is a failure.
  [[ -f "$action" ]] || continue
  action_directory="$(dirname "$action")"
  # The grep pattern matches a literal action-path reference rather than a
  # shell expansion, so it is written to survive single-quote linting.
  #
  # References are swept per invocation site rather than per distinct path,
  # because the executable-bit exemption belongs to a site. A document that
  # invokes one script through an interpreter and also invokes it directly has
  # only the interpreted site exempted; collapsing the two sites into one path
  # would let the interpreted invocation excuse the direct one.
  while IFS= read -r match; do
    site="${match%%:*}"
    reference="${match#*:}"
    relative="${reference#\$GITHUB_ACTION_PATH/}"
    resolved="$action_directory/$relative"
    checked=$((checked + 1))
    if [[ ! -f "$resolved" ]]; then
      echo "$action:$site references $reference, which does not resolve to a file" >&2
      failed=$((failed + 1))
      continue
    fi
    # A script run through an explicit interpreter does not need its own
    # executable bit; one invoked directly does. Deleting every interpreted
    # invocation from the line leaves exactly the references the shell executes
    # itself, so the question is asked of this site rather than of the document.
    direct="$(
      sed -n "${site}p" "$action" |
        sed -E 's/(^|[^[:alnum:]_])(bash|sh)[[:space:]]+"?[$]GITHUB_ACTION_PATH\/[A-Za-z0-9._\/-]*/\1/g' |
        grep -o -- '[$]GITHUB_ACTION_PATH/[A-Za-z0-9._/-]*' || true
    )"
    if ! grep -qxF -- "$reference" <<<"$direct"; then
      continue
    fi
    if [[ ! -x "$resolved" ]]; then
      echo "$action:$site references $reference, which is invoked directly but is not executable" >&2
      failed=$((failed + 1))
    fi
  done < <(grep -no -- '[$]GITHUB_ACTION_PATH/[A-Za-z0-9._/-]*' "$action" | sort -u)
done

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

if [[ "$checked" -eq 0 ]]; then
  echo "No action referenced a repository script; the assertion is vacuous." >&2
  exit 1
fi

echo "Action script references resolve from their own action directories ($checked checked)."
