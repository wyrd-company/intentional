#!/usr/bin/env bash
# ---
# relationships:
#   validates: github-release-executor
# ---
#
# Prove the composite-action lint gate rejects what it claims to reject.
#
# A gate that only ever sees compliant files cannot distinguish "the rule holds"
# from "the rule is never evaluated". Each negative case below restores a shape
# the gate exists to stop, including the exact interpolated installer step the
# published actions used before it was routed through env:.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

linter=(python3 "$root/scripts/release/lint-actions.py")
failures=0

expect_pass() {
  local description="$1"
  shift
  if "${linter[@]}" "$@" >/dev/null 2>&1; then
    return
  fi
  echo "expected the lint gate to accept $description, but it reported:" >&2
  "${linter[@]}" "$@" >&2 || true
  failures=$((failures + 1))
}

expect_failure_matching() {
  local description="$1"
  local pattern="$2"
  shift 2

  local output
  if output="$("${linter[@]}" "$@" 2>&1)"; then
    echo "expected the lint gate to reject $description, but it passed" >&2
    failures=$((failures + 1))
    return
  fi

  if ! grep -qE -- "$pattern" <<<"$output"; then
    echo "the lint gate rejected $description without reporting /$pattern/:" >&2
    echo "$output" >&2
    failures=$((failures + 1))
  fi
}

# Rewrite a published installer step back to the interpolated form it shipped
# in before review. Substituting nothing is itself a failure: it would mean the
# negative case silently stopped being exercised.
interpolate_installer() {
  local source="$1"
  local destination="$2"

  SOURCE="$source" DESTINATION="$destination" python3 - <<'PY'
import os
import sys

compliant = """      env:
        INTENTIONAL_VERSION: ${{ inputs.intentional-version }}
      run: bash "$GITHUB_ACTION_PATH/../install.sh" "$INTENTIONAL_VERSION"
"""
interpolated = (
    '      run: bash "$GITHUB_ACTION_PATH/../install.sh" '
    '"${{ inputs.intentional-version }}"\n'
)

source = os.environ["SOURCE"]
text = open(source, encoding="utf-8").read()
if compliant not in text:
    sys.exit(f"{source} no longer contains the env-routed installer step")

open(os.environ["DESTINATION"], "w", encoding="utf-8").write(
    text.replace(compliant, interpolated)
)
PY
}

expect_pass "the repository's own action documents"

for action in actions/contribute/action.yml actions/assemble-evidence/action.yml; do
  expect_pass "$action" "$action"

  reverted="$temporary/$(basename "$(dirname "$action")")-interpolated.yml"
  interpolate_installer "$action" "$reverted"
  expect_failure_matching \
    "$action with its installer step interpolated" \
    "interpolates a .* expansion into its run" \
    "$reverted"
done

cat > "$temporary/missing-shell.yml" <<'FIXTURE'
name: "fixture"
description: "A run step that never declares a shell"
runs:
  using: "composite"
  steps:
    - name: Report
      run: echo done
FIXTURE

expect_failure_matching \
  "a run step with no shell" \
  "run: without a shell" \
  "$temporary/missing-shell.yml"

cat > "$temporary/unsupported-shell.yml" <<'FIXTURE'
name: "fixture"
description: "A run step under a shell the repository does not use"
runs:
  using: "composite"
  steps:
    - name: Report
      shell: pwsh
      run: Write-Output done
FIXTURE

expect_failure_matching \
  "a run step under an unsupported shell" \
  "this repository supports only bash" \
  "$temporary/unsupported-shell.yml"

cat > "$temporary/floating-reference.yml" <<'FIXTURE'
name: "fixture"
description: "An external action resolved by a mutable name"
runs:
  using: "composite"
  steps:
    - name: Check out
      uses: actions/checkout@v4
FIXTURE

expect_failure_matching \
  "an external action pinned to a tag" \
  "not pinned to a complete 40-character commit identity" \
  "$temporary/floating-reference.yml"

cat > "$temporary/compliant.yml" <<'FIXTURE'
name: "fixture"
description: "The same values routed the way the gate requires"
inputs:
  version:
    description: "A value supplied by the caller"
    required: true
runs:
  using: "composite"
  steps:
    - name: Check out
      uses: actions/checkout@08c6903cd8c0fde910a37f88322edcfb5dd907a8 # v5
    - name: Report
      shell: bash
      env:
        INPUT_VERSION: ${{ inputs.version }}
      run: echo "$INPUT_VERSION"
FIXTURE

expect_pass "an env-routed, pinned, bash composite step" "$temporary/compliant.yml"

if [[ "$failures" -ne 0 ]]; then
  echo "$failures composite-action lint assertions failed." >&2
  exit 1
fi

echo "Composite-action lint gate accepts compliant actions and rejects each violation."
