#!/usr/bin/env bash
# ---
# relationships:
#   validates: github-release-executor
# ---
#
# Prove the online Action-pin check is complete and resilient.
#
# The check is the only seam that can notice an external Action pin disagreeing
# with its upstream tag, so two properties matter and neither is visible from a
# green run against a well-formed declaration:
#
#   completeness -- every declared entry is read, or the run fails naming the
#   entry it could not read. A count of what was read is not evidence of what
#   was declared.
#
#   resilience   -- a failed `git ls-remote` is retried to a stated ceiling,
#   and a resolved-commit DISAGREEMENT is never retried. Retrying the one real
#   finding this seam exists to raise would convert it into silence.
#
# Every case runs the real script against a stub `git` on PATH that logs its own
# arguments, so what is asserted is the recorded invocation rather than the
# source text.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
check="$root/scripts/release/verify-github-action-pins-online.py"

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

failures=0
case_number=0

# One well-formed declaration carrying a single entry.
#
# Cases that are about completeness rewrite this text; cases that are about
# retry leave it alone and move the stub instead.
declaration() {
  cat <<'YAML'
actions:
  - constant: EXAMPLE_ACTION
    repository: example-owner/example-action
    tag: v1.0.0
    commit: 1111111111111111111111111111111111111111
YAML
}

# Install a stub `git` that logs every invocation and behaves as instructed.
#
# `mode` is the behaviour the stub performs; `payload` is the commit it reports
# when it reports one. The log is one line per invocation, so a retry is visible
# as a repeated line rather than inferred from an exit status.
stub_git() {
  local directory="$1"
  local mode="$2"
  local payload="${3:-}"

  mkdir -p "$directory/bin"
  cat >"$directory/bin/git" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$directory/git.log"
attempts="\$(wc -l <"$directory/git.log")"
case "$mode" in
  fail-twice-then-succeed)
    if [[ "\$attempts" -le 2 ]]; then
      echo "stub: transient failure" >&2
      exit 128
    fi
    ;;
  fail-always)
    echo "stub: transient failure" >&2
    exit 128
    ;;
esac
printf '%s\trefs/tags/v1.0.0\n' "$payload"
STUB
  chmod +x "$directory/bin/git"
}

# Run the check against one declaration and one stub, returning its exit status.
run_check() {
  local directory="$1"
  local declaration_file="$2"

  # The retry delay is the one thing a test cannot afford to honour, so the
  # ceiling is exercised at zero delay. The attempt count is NOT overridden:
  # the number asserted below is the production ceiling.
  PATH="$directory/bin:$PATH" \
    INTENTIONAL_ACTION_PIN_RETRY_DELAY=0 \
    python3 "$check" "$declaration_file" >"$directory/stdout" 2>"$directory/stderr"
}

# Count the stub invocations naming one repository.
invocations_for() {
  local directory="$1"
  local repository="$2"
  grep -c -- "$repository" "$directory/git.log" 2>/dev/null || true
}

new_case() {
  local name="$1"
  local directory="$temporary/$name"
  mkdir -p "$directory"
  printf '%s' "$directory"
}

report() {
  echo "  stdout: $(cat "$1/stdout" 2>/dev/null)" >&2
  echo "  stderr: $(cat "$1/stderr" 2>/dev/null)" >&2
  echo "  git.log:" >&2
  sed 's/^/    /' "$1/git.log" >&2 2>/dev/null || true
  failures=$((failures + 1))
}

# ---------------------------------------------------------------------------
# Completeness
# ---------------------------------------------------------------------------

# The recorded witness: double-spacing one entry's field values makes the field
# pattern miss those lines, so the entry is never yielded and never mentioned.
case_number=$((case_number + 1))
directory="$(new_case double-spaced)"
stub_git "$directory" succeed 1111111111111111111111111111111111111111
{
  declaration
  cat <<'YAML'
  - constant: SKIPPED_ACTION
    repository:  example-owner/skipped-action
    tag:  v2.0.0
    commit:  2222222222222222222222222222222222222222
YAML
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a double-spaced entry to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "SKIPPED_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the check refused the double-spaced declaration without naming SKIPPED_ACTION" >&2
  report "$directory"
fi

# Corrupt the PARSED side alone: one entry loses a field the parser needs, so
# the parse yields fewer entries than the text declares. The raw count is
# untouched, so only an equality between the two can notice.
case_number=$((case_number + 1))
directory="$(new_case parsed-side-corrupted)"
stub_git "$directory" succeed 1111111111111111111111111111111111111111
{
  declaration
  cat <<'YAML'
  - constant: HALF_READ_ACTION
    repository: example-owner/half-read-action
    tag: v3.0.0
YAML
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry the parser cannot complete to fail the check, but it passed" >&2
  report "$directory"
fi

# Corrupt the RAW side alone: an entry header the raw count sees and the field
# parser cannot attach to anything. The parse is untouched.
case_number=$((case_number + 1))
directory="$(new_case raw-side-corrupted)"
stub_git "$directory" succeed 1111111111111111111111111111111111111111
{
  declaration
  cat <<'YAML'
  - constant: HEADER_ONLY_ACTION
YAML
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry header with no fields to fail the check, but it passed" >&2
  report "$directory"
fi

# A well-formed declaration still passes, so the equality is not simply always
# false.
case_number=$((case_number + 1))
directory="$(new_case complete)"
stub_git "$directory" succeed 1111111111111111111111111111111111111111
declaration >"$directory/pins.yml"
if ! run_check "$directory" "$directory/pins.yml"; then
  echo "expected a complete declaration to pass the check, but it failed" >&2
  report "$directory"
fi

# ---------------------------------------------------------------------------
# Resilience
# ---------------------------------------------------------------------------

# A transient failure is retried and the run succeeds.
case_number=$((case_number + 1))
directory="$(new_case retry-recovers)"
stub_git "$directory" fail-twice-then-succeed 1111111111111111111111111111111111111111
declaration >"$directory/pins.yml"
if ! run_check "$directory" "$directory/pins.yml"; then
  echo "expected two transient failures to be retried to success, but the check failed" >&2
  report "$directory"
elif [[ "$(invocations_for "$directory" example-owner/example-action)" -ne 3 ]]; then
  echo "expected three invocations for the retried repository, got $(invocations_for "$directory" example-owner/example-action)" >&2
  report "$directory"
fi

# Retry is bounded. The ceiling asserted here is the production attempt count.
case_number=$((case_number + 1))
directory="$(new_case retry-ceiling)"
stub_git "$directory" fail-always 1111111111111111111111111111111111111111
declaration >"$directory/pins.yml"
ceiling="$(grep -oE '^ATTEMPTS = [0-9]+' "$check" | grep -oE '[0-9]+' || true)"
if [[ -z "$ceiling" ]]; then
  echo "the check states no ATTEMPTS ceiling to hold it to" >&2
  failures=$((failures + 1))
elif run_check "$directory" "$directory/pins.yml"; then
  echo "expected an always-failing resolution to fail the check, but it passed" >&2
  report "$directory"
elif [[ "$(invocations_for "$directory" example-owner/example-action)" -ne "$ceiling" ]]; then
  echo "expected exactly $ceiling invocations at the retry ceiling, got $(invocations_for "$directory" example-owner/example-action)" >&2
  report "$directory"
fi

# The case that matters. A resolved commit that DISAGREES is the finding this
# seam exists to raise. It is not a process failure, so it is never retried:
# exactly one invocation, and a non-zero exit.
case_number=$((case_number + 1))
directory="$(new_case disagreement-is-not-retried)"
stub_git "$directory" succeed 9999999999999999999999999999999999999999
declaration >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a disagreeing resolved commit to fail the check, but it passed" >&2
  report "$directory"
elif [[ "$(invocations_for "$directory" example-owner/example-action)" -ne 1 ]]; then
  echo "a disagreement was re-resolved $(invocations_for "$directory" example-owner/example-action) times; retry must never re-run a disagreement" >&2
  report "$directory"
fi

# The ceiling asserted above is read out of the check itself, so it proves the
# loop honours whatever it states and cannot notice the number changing. The
# number is a claim of its own -- the check's docstring says it follows the
# bounded-attempt shape the publication scripts already use -- so it is held to
# that convention rather than to itself.
case_number=$((case_number + 1))
convention="$(grep -oE 'for _attempt in \{1\.\.[0-9]+\}' "$root/scripts/release/ensure-npm.sh" | grep -oE '[0-9]+' | tail -1 || true)"
if [[ -z "$convention" ]]; then
  echo "scripts/release/ensure-npm.sh no longer states the retry convention to hold the check to" >&2
  failures=$((failures + 1))
elif [[ "$ceiling" != "$convention" ]]; then
  echo "the check retries $ceiling times but the repository convention is $convention attempts" >&2
  failures=$((failures + 1))
fi

if [[ "$failures" -ne 0 ]]; then
  echo "$failures of $case_number Action-pin evidence cases did not behave as stated." >&2
  exit 1
fi

echo "The Action-pin check is complete and resilient as stated ($case_number cases)."
