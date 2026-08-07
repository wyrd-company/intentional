#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="${1:-$root/.github/workflows/ci.yml}"

if ! command -v yq >/dev/null 2>&1; then
  echo "the hosted task ci assertion requires yq, but it is unavailable" >&2
  exit 1
fi

hosted_task_calls="$(
  yq -r '[.jobs[] | .steps[]? | select(.run == "task ci")] | length' \
    "$workflow"
)"
if [[ "$hosted_task_calls" -ne 1 ]]; then
  echo "hosted CI must invoke task ci exactly once; found $hosted_task_calls calls" >&2
  exit 1
fi

if ! yq -e '.on | has("pull_request")' "$workflow" >/dev/null; then
  echo "hosted CI must declare the pull_request trigger" >&2
  exit 1
fi

hosted_job="$(
  yq -r \
    '.jobs | to_entries[] | select([.value.steps[]? | select(.run == "task ci")] | length == 1) | .key' \
    "$workflow"
)"
if ! CI_JOB="$hosted_job" yq -e \
  '.jobs[strenv(CI_JOB)] | has("if") | not' "$workflow" >/dev/null; then
  echo "the hosted task ci job must remain unconditional for pull requests: $hosted_job" >&2
  exit 1
fi

echo "Hosted CI runs the unconditional task ci contract for pull requests."
