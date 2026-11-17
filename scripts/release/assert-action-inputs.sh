#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

# Guards the GitHub Action's public inputs. Every input a released Action
# accepted must still exist, reach the run step, and reach the command line.
# Removing one breaks adopters' workflows, so it needs a major release and a
# deliberate edit to the released list below.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
action="$root/action.yml"

# Inputs accepted by the released Action (0.4.1). Add new inputs here once they
# ship; remove one only with the major release that drops it.
released_inputs=(
  command
  channel
  prerelease
  phase
  release-plan
  baseline
  baseline-versions
  take-over
  json
  dry-run
  working-directory
  intentional-version
)

fail() {
  echo "action input check failed: $*" >&2
  exit 1
}

for input in "${released_inputs[@]}"; do
  [[ "$(yq ".inputs | has(\"$input\")" "$action")" == "true" ]] ||
    fail "action.yml no longer declares released input '$input'"

  # intentional-version is consumed by the install step, not the run step.
  [[ "$input" == intentional-version ]] && continue
  variable="INPUT_$(tr '[:lower:]-' '[:upper:]_' <<<"$input")"
  expected="\${{ inputs.$input }}"
  actual="$(yq ".runs.steps[] | select(.id == \"run\") | .env.$variable" "$action")"
  [[ "$actual" == "$expected" ]] ||
    fail "run step must map $variable to '$expected'; found '$actual'"
done

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

yq '.runs.steps[] | select(.id == "run") | .run' "$action" >"$temporary/run.sh"
mkdir -p "$temporary/bin" "$temporary/workspace"
cat >"$temporary/bin/intentional" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >"${INTENTIONAL_STUB_LOG:?INTENTIONAL_STUB_LOG is required}"
EOF
chmod +x "$temporary/bin/intentional"

# Runs the Action's run step with the stub binary. Arguments are NAME=value
# input variables; unset inputs are empty, as GitHub passes them.
run_action() {
  : >"$temporary/stub.log"
  env -i PATH="$temporary/bin:$PATH" \
    GITHUB_WORKSPACE="$temporary/workspace" \
    GITHUB_OUTPUT="$temporary/output" \
    INTENTIONAL_STUB_LOG="$temporary/stub.log" \
    INPUT_COMMAND=check INPUT_CHANNEL= INPUT_PRERELEASE= INPUT_PHASE= \
    INPUT_RELEASE_PLAN= INPUT_BASELINE=false INPUT_BASELINE_VERSIONS= \
    INPUT_TAKE_OVER=false INPUT_JSON=false INPUT_DRY_RUN=false \
    INPUT_WORKING_DIRECTORY=. \
    "$@" bash "$temporary/run.sh"
}

run_action INPUT_COMMAND=tag INPUT_PHASE=before-publication >/dev/null ||
  fail "tag with phase was rejected"
invocation="$(cat "$temporary/stub.log")"
[[ "$invocation" == "--directory . tag --phase before-publication" ]] ||
  fail "tag with phase must invoke 'intentional --directory . tag --phase before-publication'; got '$invocation'"

if output="$(run_action INPUT_COMMAND=check INPUT_PHASE=after-publication 2>&1)"; then
  fail "phase must be rejected for commands other than tag"
fi
[[ "$output" == *"phase is only valid for tag."* ]] ||
  fail "phase on check must explain that it is only valid for tag; got '$output'"
[[ ! -s "$temporary/stub.log" ]] ||
  fail "phase on check must not run intentional"

echo "Released GitHub Action inputs are declared and passed through."
