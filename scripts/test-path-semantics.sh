#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---
#
# Run the path-semantics tests: each core unit-test group, then the
# shared-projection end-to-end tests. A group that runs no tests fails, so
# that a renamed module cannot drop its coverage while the job stays green.
#
# Usage: scripts/test-path-semantics.sh [group...]

set -euo pipefail

if [ "$#" -gt 0 ]; then
  groups=("$@")
else
  groups=(projection_file:: config::tests:: ownership:: workspace_files::)
fi

for group in "${groups[@]}"; do
  if ! output=$(cargo test --locked -p intentional-core --lib -- "$group" 2>&1); then
    printf '%s\n' "$output"
    exit 1
  fi
  printf '%s\n' "$output"
  if ! printf '%s\n' "$output" | grep -Eq '^test result: ok\. [1-9][0-9]* passed'; then
    echo "test group $group ran no tests" >&2
    exit 1
  fi
done

cargo test --locked -p intentional-cli --test shared_projections
