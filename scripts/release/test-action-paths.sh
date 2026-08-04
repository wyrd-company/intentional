#!/usr/bin/env bash
# ---
# relationships:
#   validates: github-release-executor
# ---
#
# Prove the action-path gate rejects what it claims to reject.
#
# The gate asserts three things: every referenced path resolves from the
# action's own directory, a directly invoked script carries an executable bit,
# and a sweep that finds nothing is a failure rather than a pass. Each is
# restored below as the shape the gate exists to stop, and each negative case
# must fail on its own so a single over-broad rule cannot stand in for three.

# Every case body below spells `$GITHUB_ACTION_PATH` literally, because it is
# the text the gate reads out of an action document rather than a value this
# script expands. Letting it expand would write an empty path into the fixture
# and leave the gate examining nothing.
# shellcheck disable=SC2016

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
gate="$root/scripts/release/assert-action-paths.sh"

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

failures=0
case_number=0

# Build a tree holding one composite action and one shared script.
#
# The script's executable bit and the action's `runs:` body are the two
# variables every case below moves, so they are the two the caller supplies.
build_tree() {
  local name="$1"
  local executable="$2"
  local body="$3"
  local tree="$temporary/$name"

  rm -rf "$tree"
  mkdir -p "$tree/actions/example" "$tree/scripts"
  printf '#!/usr/bin/env bash\ntrue\n' >"$tree/scripts/shared.sh"
  chmod "$executable" "$tree/scripts/shared.sh"
  cat >"$tree/actions/example/action.yml" <<YAML
name: example
runs:
  using: composite
  steps:
    - shell: bash
      run: |
$body
YAML
  printf '%s' "$tree"
}

expect_pass() {
  local description="$1"
  local tree="$2"
  case_number=$((case_number + 1))
  if "$gate" "$tree" >/dev/null 2>&1; then
    return
  fi
  echo "expected the action-path gate to accept $description, but it reported:" >&2
  "$gate" "$tree" >&2 || true
  failures=$((failures + 1))
}

expect_failure_matching() {
  local description="$1"
  local pattern="$2"
  local tree="$3"
  case_number=$((case_number + 1))

  local output
  if output="$("$gate" "$tree" 2>&1)"; then
    echo "expected the action-path gate to reject $description, but it passed:" >&2
    echo "$output" >&2
    failures=$((failures + 1))
    return
  fi

  if ! grep -qE -- "$pattern" <<<"$output"; then
    echo "the action-path gate rejected $description without reporting /$pattern/:" >&2
    echo "$output" >&2
    failures=$((failures + 1))
  fi
}

# A script reached only through an explicit interpreter needs no executable bit.
expect_pass \
  "an interpreted invocation of a script without an executable bit" \
  "$(build_tree interpreted 644 '        bash "$GITHUB_ACTION_PATH/../../scripts/shared.sh"')"

# A script invoked directly needs one, and carries it here.
expect_pass \
  "a direct invocation of an executable script" \
  "$(build_tree direct 755 '        "$GITHUB_ACTION_PATH/../../scripts/shared.sh"')"

expect_failure_matching \
  "a reference that resolves to no file" \
  "does not resolve to a file" \
  "$(build_tree unresolved 755 '        bash "$GITHUB_ACTION_PATH/../../scripts/absent.sh"')"

expect_failure_matching \
  "a direct invocation of a script without an executable bit" \
  "invoked directly but is not executable" \
  "$(build_tree unexecutable 644 '        "$GITHUB_ACTION_PATH/../../scripts/shared.sh"')"

expect_failure_matching \
  "an action tree that references no repository script at all" \
  "the assertion is vacuous" \
  "$(build_tree vacuous 755 '        true')"

# The residual this gate carried: the exemption belonged to the pair of action
# document and script path rather than to the invocation site, so one
# interpreted invocation anywhere in the document excused every direct
# invocation of the same script elsewhere in it.
expect_failure_matching \
  "a script invoked both through an interpreter and directly, without an executable bit" \
  "invoked directly but is not executable" \
  "$(build_tree mixed 644 '        bash "$GITHUB_ACTION_PATH/../../scripts/shared.sh"
        "$GITHUB_ACTION_PATH/../../scripts/shared.sh"')"

if [[ "$failures" -ne 0 ]]; then
  echo "$failures of $case_number action-path gate cases did not behave as stated." >&2
  exit 1
fi

echo "The action-path gate accepts and rejects as stated ($case_number cases)."
