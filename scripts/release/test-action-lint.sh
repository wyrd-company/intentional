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

cat > "$temporary/unknown-runtime.yml" <<'FIXTURE'
name: "fixture"
description: "A runtime this gate has never been taught to read"
runs:
  using: "node12"
  main: "index.js"
FIXTURE

expect_failure_matching \
  "an unrecognised runs.using" \
  "declares runs.using 'node12', which this gate does not recognise" \
  "$temporary/unknown-runtime.yml"

# `using: composite` is matched case-insensitively by GitHub. Reading it any
# other way turns a capital letter into a silent exemption from every step rule.
cat > "$temporary/capitalised-composite.yml" <<'FIXTURE'
name: "fixture"
description: "A violating step under a capitalised runtime name"
runs:
  using: "Composite"
  steps:
    - name: Report
      run: echo done
FIXTURE

expect_failure_matching \
  "a violating step under using: Composite" \
  "run: without a shell" \
  "$temporary/capitalised-composite.yml"

cat > "$temporary/floating-container.yml" <<'FIXTURE'
name: "fixture"
description: "A container step resolved by a mutable tag"
runs:
  using: "composite"
  steps:
    - name: Convert
      uses: docker://alpine:3.20
FIXTURE

expect_failure_matching \
  "a container step pinned to a tag" \
  "not pinned to an image digest" \
  "$temporary/floating-container.yml"

# The commit-identity message belongs to commit-shaped references. Reporting it
# against an image would tell the reader to pin to something images do not have.
container_output="$("${linter[@]}" "$temporary/floating-container.yml" 2>&1 || true)"
if grep -qF -- "40-character commit identity" <<<"$container_output"; then
  echo "the lint gate told a container reference to pin to a commit identity:" >&2
  echo "$container_output" >&2
  failures=$((failures + 1))
fi

digest="$(printf '0%.0s' $(seq 64))"
cat > "$temporary/pinned-container.yml" <<FIXTURE
name: "fixture"
description: "A container step pinned to its manifest digest"
runs:
  using: "composite"
  steps:
    - name: Convert
      uses: docker://alpine@sha256:$digest
FIXTURE

expect_pass "a digest-pinned container step" "$temporary/pinned-container.yml"

# Discovery is the property the explicit-path cases above cannot exercise: they
# hand the gate the file. Copying the gate into a fixture repository lets it
# discover for itself, and lets the expected document count be an exact number.
fixture_repository() {
  local repository="$1"

  mkdir -p \
    "$repository/scripts/release" \
    "$repository/actions/published" \
    "$repository/.github/actions/internal" \
    "$repository/target/generated" \
    "$repository/node_modules/vendored"
  cp "$root/scripts/release/lint-actions.py" "$repository/scripts/release/lint-actions.py"

  for document in \
    "$repository/action.yml" \
    "$repository/actions/published/action.yml" \
    "$repository/.github/actions/internal/action.yaml" \
    "$repository/target/generated/action.yml" \
    "$repository/node_modules/vendored/action.yml"; do
    cp "$temporary/compliant.yml" "$document"
  done
}

discovered="$temporary/discovered"
fixture_repository "$discovered"

# Three documents are publishable; the two under target/ and node_modules/ are
# not this repository's to fix. Asserting the number, not just the exit status,
# means a future narrowing of discovery fails loudly instead of passing over
# fewer files.
discovered_output="$(python3 "$discovered/scripts/release/lint-actions.py" 2>&1)" || {
  echo "expected the lint gate to accept the discovery fixture, but it reported:" >&2
  echo "$discovered_output" >&2
  failures=$((failures + 1))
}

if ! grep -qF -- "(3 checked)" <<<"$discovered_output"; then
  echo "expected the lint gate to discover 3 action documents, but it reported:" >&2
  echo "$discovered_output" >&2
  failures=$((failures + 1))
fi

# The same discovery, proven by what it rejects: an interpolated run: in an
# `action.yaml` under `.github/actions/` is a file the previous gate never
# opened.
uncovered="$temporary/uncovered"
fixture_repository "$uncovered"
cat > "$uncovered/.github/actions/internal/action.yaml" <<'FIXTURE'
name: "fixture"
description: "An interpolated run: in a spelling and a directory the gate must reach"
inputs:
  version:
    description: "A value supplied by the caller"
    required: true
runs:
  using: "composite"
  steps:
    - name: Report
      shell: bash
      run: echo "${{ inputs.version }}"
FIXTURE

if uncovered_output="$(python3 "$uncovered/scripts/release/lint-actions.py" 2>&1)"; then
  echo "expected the lint gate to reject the .github/actions/ action.yaml, but it passed" >&2
  failures=$((failures + 1))
elif ! grep -qE -- "\.github/actions/internal/action\.yaml.*interpolates" <<<"$uncovered_output"; then
  echo "the lint gate rejected the discovery fixture without naming the uncovered document:" >&2
  echo "$uncovered_output" >&2
  failures=$((failures + 1))
fi

# The repository's own count, cross-checked against an enumeration the gate does
# not perform. A narrowing that both sides share would still pass here; the
# fixture above is what holds the exact number.
expected_documents="$(
  {
    find . -maxdepth 1 -type f \( -name action.yml -o -name action.yaml \)
    find ./actions ./.github/actions \
      \( -name target -o -name node_modules \) -prune -o \
      -type f \( -name action.yml -o -name action.yaml \) -print 2>/dev/null || true
  } | wc -l
)"

repository_output="$("${linter[@]}")"
if ! grep -qF -- "($expected_documents checked)" <<<"$repository_output"; then
  echo "expected the lint gate to check $expected_documents documents, but it reported:" >&2
  echo "$repository_output" >&2
  failures=$((failures + 1))
fi

if [[ "$failures" -ne 0 ]]; then
  echo "$failures composite-action lint assertions failed." >&2
  exit 1
fi

echo "Composite-action lint gate accepts compliant actions and rejects each violation."
