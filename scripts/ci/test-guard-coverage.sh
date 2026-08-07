#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

task_ci_job() {
  yq -r \
    '.jobs | to_entries[] | select([.value.steps[]? | select(.run == "task ci")] | length == 1) | .key' \
    "$1"
}

if ! "$root/scripts/ci/assert-hosted-task-ci.sh"; then
  echo "the repository hosted task ci contract failed" >&2
  exit 1
fi

cp "$root/.github/workflows/ci.yml" "$temporary/without-task-ci.yml"
source_job="$(task_ci_job "$temporary/without-task-ci.yml")"
SOURCE_JOB="$source_job" yq -i \
  '.jobs."renamed-contract" = .jobs[strenv(SOURCE_JOB)] |
   del(.jobs[strenv(SOURCE_JOB)])' \
  "$temporary/without-task-ci.yml"
contract_job="$(task_ci_job "$temporary/without-task-ci.yml")"
CI_JOB="$contract_job" yq -i \
  '(.jobs[strenv(CI_JOB)].steps[] | select(.run == "task ci").run) = "task test"' \
  "$temporary/without-task-ci.yml"
if "$root/scripts/ci/assert-hosted-task-ci.sh" \
  "$temporary/without-task-ci.yml" \
  >"$temporary/without-task-ci.stdout" \
  2>"$temporary/without-task-ci.stderr"; then
  echo "hosted CI without task ci passed its production assertion" >&2
  exit 1
elif ! grep -Fq 'hosted CI must invoke task ci exactly once; found 0 calls' \
  "$temporary/without-task-ci.stderr"; then
  echo "hosted CI without task ci failed for the wrong reason" >&2
  cat "$temporary/without-task-ci.stderr" >&2
  exit 1
fi

cp "$root/.github/workflows/ci.yml" "$temporary/without-pull-request.yml"
yq -i 'del(.on.pull_request)' "$temporary/without-pull-request.yml"
if "$root/scripts/ci/assert-hosted-task-ci.sh" \
  "$temporary/without-pull-request.yml" \
  >"$temporary/without-pull-request.stdout" \
  2>"$temporary/without-pull-request.stderr"; then
  echo "hosted CI without a pull_request trigger passed its production assertion" >&2
  exit 1
elif ! grep -Fq 'hosted CI must declare the pull_request trigger' \
  "$temporary/without-pull-request.stderr"; then
  echo "hosted CI without a pull_request trigger failed for the wrong reason" >&2
  cat "$temporary/without-pull-request.stderr" >&2
  exit 1
fi

for filter in branches branches-ignore paths paths-ignore; do
  filtered_workflow="$temporary/pull-request-$filter-filtered.yml"
  cp "$root/.github/workflows/ci.yml" "$filtered_workflow"
  FILTER="$filter" yq -i \
    '.on.pull_request[strenv(FILTER)] = ["never-selected"]' \
    "$filtered_workflow"
  if "$root/scripts/ci/assert-hosted-task-ci.sh" "$filtered_workflow" \
    >"$temporary/pull-request-$filter-filtered.stdout" \
    2>"$temporary/pull-request-$filter-filtered.stderr"; then
    echo "hosted CI with a pull_request $filter filter passed its production assertion" >&2
    exit 1
  elif ! grep -Fq \
    'hosted CI must not filter pull requests by branch, path, or activity' \
    "$temporary/pull-request-$filter-filtered.stderr"; then
    echo "the pull_request $filter filter failed for the wrong reason" >&2
    cat "$temporary/pull-request-$filter-filtered.stderr" >&2
    exit 1
  fi
done

cp "$root/.github/workflows/ci.yml" "$temporary/pull-request-excluded.yml"
contract_job="$(task_ci_job "$temporary/pull-request-excluded.yml")"
if [[ "$contract_job" != "shared-contract" ]]; then
  SOURCE_JOB="$contract_job" yq -i \
    '.jobs."shared-contract" = .jobs[strenv(SOURCE_JOB)] |
     del(.jobs[strenv(SOURCE_JOB)])' \
    "$temporary/pull-request-excluded.yml"
fi
contract_job="$(task_ci_job "$temporary/pull-request-excluded.yml")"
# Preserve the GitHub expression literally in the mutated workflow.
# shellcheck disable=SC2016
JOB_IF="\${{ github.event_name != 'pull_request' }}" \
  CI_JOB="$contract_job" yq -i \
  '.jobs[strenv(CI_JOB)].if = strenv(JOB_IF)' \
  "$temporary/pull-request-excluded.yml"
if "$root/scripts/ci/assert-hosted-task-ci.sh" \
  "$temporary/pull-request-excluded.yml" \
  >"$temporary/pull-request-excluded.stdout" \
  2>"$temporary/pull-request-excluded.stderr"; then
  echo "a task ci job that excludes pull requests passed its production assertion" >&2
  exit 1
elif ! grep -Fq \
  "the hosted task ci job must remain unconditional for pull requests: $contract_job" \
  "$temporary/pull-request-excluded.stderr"; then
  echo "the pull-request-excluding task ci job failed for the wrong reason" >&2
  cat "$temporary/pull-request-excluded.stderr" >&2
  exit 1
fi

cp "$root/.github/workflows/ci.yml" "$temporary/task-step-excludes-pull-request.yml"
contract_job="$(task_ci_job "$temporary/task-step-excludes-pull-request.yml")"
# Preserve the GitHub expression literally in the mutated workflow.
# shellcheck disable=SC2016
STEP_IF="\${{ github.event_name != 'pull_request' }}" \
  CI_JOB="$contract_job" yq -i \
  '(.jobs[strenv(CI_JOB)].steps[] | select(.run == "task ci")).if = strenv(STEP_IF)' \
  "$temporary/task-step-excludes-pull-request.yml"
if "$root/scripts/ci/assert-hosted-task-ci.sh" \
  "$temporary/task-step-excludes-pull-request.yml" \
  >"$temporary/task-step-excludes-pull-request.stdout" \
  2>"$temporary/task-step-excludes-pull-request.stderr"; then
  echo "a task ci step that excludes pull requests passed its production assertion" >&2
  exit 1
elif ! grep -Fq \
  "the hosted task ci step must remain unconditional for pull requests: $contract_job" \
  "$temporary/task-step-excludes-pull-request.stderr"; then
  echo "the pull-request-excluding task ci step failed for the wrong reason" >&2
  cat "$temporary/task-step-excludes-pull-request.stderr" >&2
  exit 1
fi

if ! "$root/scripts/ci/assert-ryl-rule-baseline.sh"; then
  echo "the repository RYL rule baseline is incomplete" >&2
  exit 1
fi

sed '/^# excluded-rule: truthy - /d' "$root/.ryl.toml" \
  >"$temporary/disabled-ryl-rule.toml"
printf '\n[rules.truthy]\nlevel = "disabled"\n' \
  >>"$temporary/disabled-ryl-rule.toml"
if "$root/scripts/ci/assert-ryl-rule-baseline.sh" \
  "$temporary/disabled-ryl-rule.toml" \
  >"$temporary/disabled-ryl-rule.stdout" \
  2>"$temporary/disabled-ryl-rule.stderr"; then
  echo "a RYL rule disabled without rationale passed its production assertion" >&2
  exit 1
elif ! grep -Fq \
  'RYL rules disabled in configuration require exclusion rationale: truthy' \
  "$temporary/disabled-ryl-rule.stderr"; then
  echo "the RYL rule disabled without rationale failed for the wrong reason" >&2
  cat "$temporary/disabled-ryl-rule.stderr" >&2
  exit 1
fi

sed '/^# excluded-rule: truthy - /d' "$root/.ryl.toml" \
  >"$temporary/unclassified-ryl-rule.toml"
if "$root/scripts/ci/assert-ryl-rule-baseline.sh" \
  "$temporary/unclassified-ryl-rule.toml" \
  >"$temporary/unclassified-ryl-rule.stdout" \
  2>"$temporary/unclassified-ryl-rule.stderr"; then
  echo "an unclassified shipped RYL rule passed its production assertion" >&2
  exit 1
elif ! grep -Fq 'unclassified shipped RYL rules: truthy' \
  "$temporary/unclassified-ryl-rule.stderr"; then
  echo "the unclassified shipped RYL rule failed for the wrong reason" >&2
  cat "$temporary/unclassified-ryl-rule.stderr" >&2
  exit 1
fi

cp "$root/.ryl.toml" "$temporary/unknown-ryl-rule.toml"
printf '%s\n' \
  '# excluded-rule: future-rule - Fixture for a rule absent from installed RYL.' \
  >>"$temporary/unknown-ryl-rule.toml"
if "$root/scripts/ci/assert-ryl-rule-baseline.sh" \
  "$temporary/unknown-ryl-rule.toml" \
  >"$temporary/unknown-ryl-rule.stdout" \
  2>"$temporary/unknown-ryl-rule.stderr"; then
  echo "an exclusion absent from installed RYL passed its production assertion" >&2
  exit 1
elif ! grep -Fq \
  'the RYL baseline classifies rules not shipped by this version: future-rule' \
  "$temporary/unknown-ryl-rule.stderr"; then
  echo "the exclusion absent from installed RYL failed for the wrong reason" >&2
  cat "$temporary/unknown-ryl-rule.stderr" >&2
  exit 1
fi

cp "$root/.ryl.toml" "$temporary/overlapping-ryl-rule.toml"
printf '%s\n' \
  '# excluded-rule: braces - Fixture for an enabled and excluded rule.' \
  >>"$temporary/overlapping-ryl-rule.toml"
if "$root/scripts/ci/assert-ryl-rule-baseline.sh" \
  "$temporary/overlapping-ryl-rule.toml" \
  >"$temporary/overlapping-ryl-rule.stdout" \
  2>"$temporary/overlapping-ryl-rule.stderr"; then
  echo "an enabled and excluded RYL rule passed its production assertion" >&2
  exit 1
elif ! grep -Fq 'RYL rules cannot be both enabled and excluded: braces' \
  "$temporary/overlapping-ryl-rule.stderr"; then
  echo "the enabled and excluded RYL rule failed for the wrong reason" >&2
  cat "$temporary/overlapping-ryl-rule.stderr" >&2
  exit 1
fi

mkdir -p "$temporary/bin"
cat >"$temporary/bin/shellcheck" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" >>"$SHELLCHECK_ROSTER"
STUB
chmod +x "$temporary/bin/shellcheck"
: >"$temporary/actual-shell-roster"

PATH="$temporary/bin:$PATH" \
  SHELLCHECK_ROSTER="$temporary/actual-shell-roster" \
  task --silent --dir "$root" shell:lint

find "$root/scripts" -type f -name '*.sh' -printf '%P\n' | sort \
  | sed 's#^#scripts/#' >"$temporary/expected-shell-roster"
sort "$temporary/actual-shell-roster" >"$temporary/sorted-actual-shell-roster"
if ! cmp "$temporary/expected-shell-roster" "$temporary/sorted-actual-shell-roster"; then
  echo "shell:lint did not pass the complete recursive shell roster to shellcheck" >&2
  diff -u "$temporary/expected-shell-roster" "$temporary/sorted-actual-shell-roster" >&2 || true
  exit 1
fi

if ! grep -Fxq 'scripts/action/observe-publication/common.sh' \
  "$temporary/sorted-actual-shell-roster"; then
  echo "shell:lint did not reach the nested publication observer scripts" >&2
  exit 1
fi

echo "Hosted CI reaches task ci on pull requests, RYL rule coverage is closed, and shell:lint reaches every shell script."
