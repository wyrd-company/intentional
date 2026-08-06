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
#   completeness -- every LINE of the declaration is accounted for, or the run
#   fails naming the line it could not read. A count of what was read is not
#   evidence of what was declared, and neither is a second count agreeing with
#   the first: both counts read the same text the same way, so an edit that
#   changes how the text is spelled moves them together. Exhaustiveness has no
#   second reading to agree with.
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

# One well-formed declaration carrying TWO entries.
#
# Two rather than one, because a single entry cannot separate the two
# properties this file exists to hold. "Retry this repository" and "re-resolve
# every repository" produce identical logs against one entry, and an
# entry-level completeness variant has no untouched remainder to be read
# correctly while the mutated entry is refused. Both cases need a second
# repository, so there is one fixture rather than two.
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
  - constant: OTHER_ACTION
    repository: other-owner/other-action
    tag: v4.0.0
    commit: 4444444444444444444444444444444444444444
YAML
}

EXAMPLE_COMMIT=1111111111111111111111111111111111111111
OTHER_COMMIT=4444444444444444444444444444444444444444

# Install a stub `git` that logs every invocation and behaves per repository.
#
# Each argument is `repository=mode:commit`. Attempts are counted per
# repository out of the log, so one repository's retries never advance
# another's, and an invocation naming a repository no behaviour was declared
# for fails loudly rather than resolving something the case did not ask for.
#
# The log is one line per invocation, so a retry is visible as a repeated line
# rather than inferred from an exit status.
stub_git() {
  local directory="$1"
  shift

  mkdir -p "$directory/bin"
  printf '%s\n' "$@" >"$directory/behaviour"
  # An empty log rather than no log, so a case that asserts the check resolved
  # NOTHING reads an empty file rather than an error it might mistake for one.
  : >"$directory/git.log"
  cat >"$directory/bin/git" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$directory/git.log"
url="\$3"
reference="\$4"
repository="\${url#https://github.com/}"
repository="\${repository%.git}"
tag="\${reference#refs/tags/}"
behaviour="\$(grep -m1 -F -- "\${repository}=" "$directory/behaviour" || true)"
if [[ -z "\$behaviour" ]]; then
  echo "stub: no behaviour declared for \$repository" >&2
  exit 127
fi
behaviour="\${behaviour#*=}"
mode="\${behaviour%%:*}"
payload="\${behaviour#*:}"
attempts="\$(grep -c -F -- "\$repository" "$directory/git.log")"
case "\$mode" in
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
printf '%s\trefs/tags/%s\n' "\$payload" "\$tag"
STUB
  chmod +x "$directory/bin/git"
}

# The stub behaviour that resolves both fixture entries to their declared pins.
# Completeness cases use it so that a case which was supposed to be refused
# fails visibly rather than by tripping over an unstubbed repository.
AGREEING=(
  "example-owner/example-action=succeed:$EXAMPLE_COMMIT"
  "other-owner/other-action=succeed:$OTHER_COMMIT"
)

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
#
# Every case below rewrites the declaration and runs the check against it. Each
# writes its own fixture and its own assertions rather than sharing a helper,
# so one instrument going wrong reddens one case rather than all of them.
# ---------------------------------------------------------------------------

# Double-spacing one entry's field values takes those lines out of the field
# shape, so the entry cannot be read.
case_number=$((case_number + 1))
directory="$(new_case double-spaced)"
stub_git "$directory" "${AGREEING[@]}"
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

# An entry that never declares one of the three fields it needs.
case_number=$((case_number + 1))
directory="$(new_case entry-missing-a-field)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration
  cat <<'YAML'
  - constant: HALF_READ_ACTION
    repository: example-owner/half-read-action
    tag: v3.0.0
YAML
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry declaring no commit to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "HALF_READ_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the check refused the incomplete entry without naming HALF_READ_ACTION" >&2
  report "$directory"
elif ! grep -q "line 10.*'  - constant: HALF_READ_ACTION'" "$directory/stderr"; then
  echo "the incomplete entry message did not identify its header as the offending line" >&2
  report "$directory"
fi

# An entry header carrying no fields at all.
case_number=$((case_number + 1))
directory="$(new_case entry-with-no-fields)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration
  cat <<'YAML'
  - constant: HEADER_ONLY_ACTION
YAML
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry header with no fields to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "HEADER_ONLY_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the check refused the empty entry without naming HEADER_ONLY_ACTION" >&2
  report "$directory"
fi

# THE WITNESS. One entry rewritten as a flow mapping is semantically identical
# YAML and passes `ryl`, so nothing else in this repository would notice it.
# Every count-based instrument this check has carried read it as a document
# with one fewer entry and reported success over the remainder.
case_number=$((case_number + 1))
directory="$(new_case entry-as-a-flow-mapping)"
stub_git "$directory" "${AGREEING[@]}"
{
  echo "actions:"
  echo "  - { constant: EXAMPLE_ACTION, repository: example-owner/example-action, tag: v1.0.0, commit: $EXAMPLE_COMMIT }"
  declaration | tail -n +6
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry written as a flow mapping to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "EXAMPLE_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the check refused the flow mapping without naming EXAMPLE_ACTION" >&2
  report "$directory"
fi

# One entry indented one space deeper than its siblings. This is not valid
# YAML -- a block sequence's items share indentation -- but the check refuses
# it on its own terms rather than by delegating to a parser, which is what
# makes the refusal available at all on a runner with no parser installed.
case_number=$((case_number + 1))
directory="$(new_case one-entry-reindented)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 5
  declaration | tail -n +6 | sed 's/^/ /'
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry indented one space deeper to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "OTHER_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the check refused the re-indented entry without naming OTHER_ACTION" >&2
  report "$directory"
fi

# The whole sequence indented one space deeper. This IS valid, semantically
# identical YAML, and it takes every entry out of the shape at once -- which is
# exactly the edit a pair of counts agrees about and misses.
case_number=$((case_number + 1))
directory="$(new_case whole-list-reindented)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 1
  declaration | tail -n +2 | sed 's/^/ /'
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a wholly re-indented sequence to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "EXAMPLE_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the re-indented sequence was refused as an empty declaration rather than an unreadable one" >&2
  report "$directory"
fi

# Fields in a different order are the same mapping, so they are read rather
# than refused. A reader that only accepts one order would fail a declaration
# nobody broke.
case_number=$((case_number + 1))
directory="$(new_case fields-reordered)"
stub_git "$directory" "${AGREEING[@]}"
{
  echo "actions:"
  echo "  - constant: EXAMPLE_ACTION"
  echo "    tag: v1.0.0"
  echo "    commit: $EXAMPLE_COMMIT"
  echo "    repository: example-owner/example-action"
  declaration | tail -n +6
} >"$directory/pins.yml"
if ! run_check "$directory" "$directory/pins.yml"; then
  echo "expected reordered fields to be read, but the check failed" >&2
  report "$directory"
elif [[ "$(invocations_for "$directory" example-owner/example-action)" -ne 1 ]]; then
  echo "the reordered entry was not resolved exactly once" >&2
  report "$directory"
fi

# A fourth key on an entry is a key this reader does not know the meaning of,
# so it is refused rather than stepped over. Reading a declaration means
# accounting for all of it.
case_number=$((case_number + 1))
directory="$(new_case entry-with-a-fourth-key)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 5
  echo "    digest: sha256:00000000000000000000000000000000000000000000000000000000000000ff"
  declaration | tail -n +6
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry carrying an unknown fourth key to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "EXAMPLE_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the check refused the fourth key without naming EXAMPLE_ACTION" >&2
  report "$directory"
fi

# A field line carrying anything beyond the value is not the field shape. The
# reader matches whole lines, so trailing content is refused rather than
# ignored -- ignoring it would let a value be qualified by text the reader
# never looked at.
case_number=$((case_number + 1))
directory="$(new_case field-with-trailing-content)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 3
  echo "    tag: v1.0.0 # the pinned release"
  declaration | tail -n +5
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a field line with trailing content to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "EXAMPLE_ACTION" "$directory/stderr" "$directory/stdout"; then
  echo "the check refused the trailing content without naming EXAMPLE_ACTION" >&2
  report "$directory"
fi

# One entry declaring a field twice. YAML resolves a duplicate key to the last
# occurrence; a reader that kept the first would verify a commit the document
# does not declare. Refusing is the only reading that cannot be wrong.
case_number=$((case_number + 1))
directory="$(new_case field-declared-twice)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 5
  echo "    commit: 5555555555555555555555555555555555555555"
  declaration | tail -n +6
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry declaring commit twice to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "commit is declared twice" "$directory/stderr"; then
  echo "the check refused the duplicated field without saying commit was declared twice" >&2
  report "$directory"
fi

# The same constant declared by two entries. Nothing downstream can tell which
# one it was handed.
case_number=$((case_number + 1))
directory="$(new_case constant-declared-twice)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration
  declaration | tail -n +6
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a constant declared twice to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "declared twice" "$directory/stderr"; then
  echo "the check refused the duplicated constant without saying so" >&2
  report "$directory"
fi

# A field line before any entry opens belongs to nothing.
case_number=$((case_number + 1))
directory="$(new_case field-outside-any-entry)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 1
  echo "    repository: example-owner/orphan-action"
  declaration | tail -n +2
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a field opening no entry to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "opens no entry" "$directory/stderr"; then
  echo "the check refused the orphaned field without saying it opens no entry" >&2
  report "$directory"
fi

# Where readers disagree about what a line is, the file is refused rather than
# resolved one way. PyYAML ends a line on U+2028 and the YAML 1.2 grammar does
# not, so this text is either one comment or five lines depending on who reads
# it -- and one of those readings declares a GHOST_ACTION nobody wrote. Neither
# reading may be adopted silently.
case_number=$((case_number + 1))
directory="$(new_case comment-carrying-a-line-separator)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration
  # U+2028 LINE SEPARATOR, four times.
  # U+2028 as its UTF-8 bytes, so the case does not depend on the locale bash
  # was started in.
  printf '%s\n' $'# stale note\xe2\x80\xa8  - constant: GHOST_ACTION\xe2\x80\xa8    repository: ghost-owner/ghost-action\xe2\x80\xa8    tag: v9.0.0\xe2\x80\xa8    commit: 8888888888888888888888888888888888888888'
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a line carrying U+2028 to fail the check, but it passed" >&2
  report "$directory"
elif grep -q "ghost-owner/ghost-action" "$directory/git.log"; then
  echo "the check resolved an entry that exists only under one reading of the file" >&2
  report "$directory"
elif ! grep -q "treat as a line break" "$directory/stderr"; then
  echo "the ambiguous line separator was refused without saying why" >&2
  report "$directory"
fi

# A second sequence key is a duplicate mapping key, and the entries under it
# would silently replace the entries above it.
case_number=$((case_number + 1))
directory="$(new_case sequence-key-reopened)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration
  echo "actions:"
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a reopened entry sequence to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "reopens" "$directory/stderr"; then
  echo "the check refused the reopened sequence without saying so" >&2
  report "$directory"
fi

# Reopening an empty sequence is still a duplicate mapping key. The first key
# need not have accumulated an entry before the second one replaces it.
case_number=$((case_number + 1))
directory="$(new_case empty-sequence-key-reopened)"
stub_git "$directory" "${AGREEING[@]}"
{
  echo "actions:"
  declaration
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an empty entry sequence to refuse reopening, but it passed" >&2
  report "$directory"
elif ! grep -q "reopens" "$directory/stderr"; then
  echo "the empty reopened sequence was refused without saying so" >&2
  report "$directory"
fi

# A wholly readable file that declares nothing. Exhaustiveness cannot notice
# this -- there is no unread line -- so it is the one thing the residual floor
# still holds, and the message says "declares no Action" rather than blaming
# an unreadable one.
case_number=$((case_number + 1))
directory="$(new_case readable-but-empty)"
stub_git "$directory" "${AGREEING[@]}"
printf 'actions:\n' >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a declaration carrying no entry to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "declares no Action" "$directory/stderr"; then
  echo "an empty declaration was not diagnosed as declaring no Action" >&2
  report "$directory"
fi

# Entries without their sequence key are not an Action pin table. They may fit
# every entry rule, but no consumer can reach them as `declaration["actions"]`.
case_number=$((case_number + 1))
directory="$(new_case sequence-key-missing)"
stub_git "$directory" "${AGREEING[@]}"
declaration | tail -n +2 >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a declaration without its actions key to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "carries no actions sequence key" "$directory/stderr"; then
  echo "the missing actions key was refused without naming what was absent" >&2
  report "$directory"
fi

# An entry header carrying anything beyond the constant is not the header
# shape. Reading the constant and stepping over the rest of the line is exactly
# the partial read this reader exists to refuse.
case_number=$((case_number + 1))
directory="$(new_case header-with-trailing-content)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 1
  echo "  - constant: EXAMPLE_ACTION deprecated"
  declaration | tail -n +3
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an entry header with trailing content to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "not a declaration this reader knows" "$directory/stderr"; then
  echo "the header with trailing content was refused for the wrong reason" >&2
  report "$directory"
fi

# The whole table written inline after the sequence key. The key line is no
# longer just the key, and reading it as one would step over every entry on it
# -- reporting an empty declaration for a file that declares plenty.
case_number=$((case_number + 1))
directory="$(new_case inline-sequence)"
stub_git "$directory" "${AGREEING[@]}"
echo "actions: [{ constant: EXAMPLE_ACTION, repository: example-owner/example-action, tag: v1.0.0, commit: $EXAMPLE_COMMIT }]" >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an inline entry sequence to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "not a declaration this reader knows" "$directory/stderr"; then
  echo "the inline sequence was diagnosed as an empty declaration rather than an unreadable line" >&2
  report "$directory"
fi

# A quoted scalar is the same string to YAML and a different string to a reader
# that takes the value as written. The quotation marks would travel into the
# resolved reference, so the value shape refuses them rather than resolving a
# tag nobody declared.
case_number=$((case_number + 1))
directory="$(new_case quoted-scalar-value)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 3
  echo '    tag: "v1.0.0"'
  declaration | tail -n +5
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected a quoted tag value to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "not a tag this reader recognises" "$directory/stderr"; then
  echo "the quoted tag value was refused for the wrong reason" >&2
  report "$directory"
fi

# An alias resolves elsewhere in the document. Reading it as the literal text
# `*anchor` would resolve a repository of that name.
case_number=$((case_number + 1))
directory="$(new_case aliased-value)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 2
  echo "    repository: *upstream"
  declaration | tail -n +4
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an aliased repository value to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "not a repository this reader recognises" "$directory/stderr"; then
  echo "the aliased repository value was refused for the wrong reason" >&2
  report "$directory"
fi

# An abbreviated commit names whatever object currently shares that prefix, so
# it is not a commit identity and this reader will not treat it as one.
case_number=$((case_number + 1))
directory="$(new_case abbreviated-commit)"
stub_git "$directory" "${AGREEING[@]}"
{
  declaration | head -n 4
  echo "    commit: 1111111"
  declaration | tail -n +6
} >"$directory/pins.yml"
if run_check "$directory" "$directory/pins.yml"; then
  echo "expected an abbreviated commit to fail the check, but it passed" >&2
  report "$directory"
elif ! grep -q "not a commit this reader recognises" "$directory/stderr"; then
  echo "the abbreviated commit was refused for the wrong reason" >&2
  report "$directory"
fi

# A CRLF checkout reads the same declaration. One trailing carriage return is
# surrendered per line, because YAML ends a line on CRLF too and a reader that
# kept it would refuse a file nobody edited.
case_number=$((case_number + 1))
directory="$(new_case crlf-line-endings)"
stub_git "$directory" "${AGREEING[@]}"
declaration | sed 's/$/\r/' >"$directory/pins.yml"
if ! run_check "$directory" "$directory/pins.yml"; then
  echo "expected a CRLF declaration to read the same, but the check failed" >&2
  report "$directory"
elif [[ "$(invocations_for "$directory" other-owner/other-action)" -ne 1 ]]; then
  echo "the CRLF declaration's second entry was not resolved" >&2
  report "$directory"
fi

# A well-formed declaration still passes, so the reader is not simply always
# refusing.
case_number=$((case_number + 1))
directory="$(new_case complete)"
stub_git "$directory" "${AGREEING[@]}"
declaration >"$directory/pins.yml"
if ! run_check "$directory" "$directory/pins.yml"; then
  echo "expected a complete declaration to pass the check, but it failed" >&2
  report "$directory"
fi

# THIS repository's real declaration is read whole. Because an unaccounted line
# is a refusal, a zero exit here IS the completeness evidence for the file the
# workflow actually checks -- no count has to be stated or compared. Nothing
# else in `task release:check` reads that file with this reader.
case_number=$((case_number + 1))
directory="$(new_case the-real-declaration)"
mkdir -p "$directory/bin"
cat >"$directory/bin/git" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$directory/git.log"
url="\$3"
reference="\$4"
repository="\${url#https://github.com/}"
repository="\${repository%.git}"
tag="\${reference#refs/tags/}"
commit="\$(awk -v r="\$repository" '\$1 == "repository:" && \$2 == r { found = 1 }
  found && \$1 == "commit:" { print \$2; exit }' "$root/github-action-pins.yml")"
printf '%s\trefs/tags/%s\n' "\$commit" "\$tag"
STUB
chmod +x "$directory/bin/git"
if ! run_check "$directory" "$root/github-action-pins.yml"; then
  echo "the repository's own Action pin declaration is not wholly readable" >&2
  report "$directory"
fi

# The verifier workflow bootstraps through external Actions before it can read
# the declaration. Every such dependency participates in the same census: each
# use must name a complete commit that agrees with the declaration.
case_number=$((case_number + 1))
while IFS= read -r use; do
  repository="${use%@*}"
  workflow_commit="${use##*@}"
  declared_commit="$(awk -v repository="$repository" '
    $1 == "repository:" && $2 == repository { found = 1; next }
    found && $1 == "commit:" { print $2; exit }
    found && $1 == "-" { exit }
  ' "$root/github-action-pins.yml")"
  if [[ ! "$workflow_commit" =~ ^[0-9a-f]{40}$ ]]; then
    echo "the verifier workflow uses $use instead of a complete commit" >&2
    failures=$((failures + 1))
  elif [[ -z "$declared_commit" ]]; then
    echo "the verifier workflow uses undeclared Action $repository" >&2
    failures=$((failures + 1))
  elif [[ "$workflow_commit" != "$declared_commit" ]]; then
    echo "the verifier workflow use $use does not match declared commit $declared_commit" >&2
    failures=$((failures + 1))
  fi
done < <(yq -r '.jobs[] | .steps[]? | select(has("uses")) | .uses' \
  "$root/.github/workflows/github-action-pins.yml")

# ---------------------------------------------------------------------------
# Resilience
# ---------------------------------------------------------------------------

# A transient failure is retried and the run succeeds -- and the retry is
# scoped to the repository that failed. The healthy repository's count is
# asserted separately from the flaky one's. Re-resolving entries in place or
# aborting and restarting the sweep both redden the healthy count while leaving
# the flaky one intact.
case_number=$((case_number + 1))
directory="$(new_case retry-is-scoped-to-one-repository)"
stub_git "$directory" \
  "example-owner/example-action=succeed:$EXAMPLE_COMMIT" \
  "other-owner/other-action=fail-twice-then-succeed:$OTHER_COMMIT"
declaration >"$directory/pins.yml"
if ! run_check "$directory" "$directory/pins.yml"; then
  echo "expected two transient failures to be retried to success, but the check failed" >&2
  report "$directory"
else
  if [[ "$(invocations_for "$directory" other-owner/other-action)" -ne 3 ]]; then
    echo "expected three invocations for the retried repository, got $(invocations_for "$directory" other-owner/other-action)" >&2
    report "$directory"
  fi
  if [[ "$(invocations_for "$directory" example-owner/example-action)" -ne 1 ]]; then
    echo "the healthy repository was resolved $(invocations_for "$directory" example-owner/example-action) times; one repository's retry must not re-resolve another" >&2
    report "$directory"
  fi
fi

# Retry is bounded. The ceiling asserted here is the production attempt count.
case_number=$((case_number + 1))
directory="$(new_case retry-ceiling)"
stub_git "$directory" \
  "example-owner/example-action=fail-always:$EXAMPLE_COMMIT" \
  "other-owner/other-action=succeed:$OTHER_COMMIT"
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
# exactly one invocation for that repository, and a non-zero exit. The second
# repository agrees, so the disagreement is what fails the run.
case_number=$((case_number + 1))
directory="$(new_case disagreement-is-not-retried)"
stub_git "$directory" \
  "example-owner/example-action=succeed:9999999999999999999999999999999999999999" \
  "other-owner/other-action=succeed:$OTHER_COMMIT"
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

# The retry ceiling and the workflow's wall-clock bound are an agreement: every
# declared repository may spend the full ceiling, and the job must outlast the
# sum. Nothing else notices when one of the three numbers moves.
case_number=$((case_number + 1))
budget="$(grep -oE '^    timeout-minutes: [0-9]+' "$root/.github/workflows/github-action-pins.yml" | grep -oE '[0-9]+' || true)"
delay="$(grep -oE '^DELAY_SECONDS = float\(os\.environ\.get\("[A-Z_]+", "[0-9]+"\)\)' "$check" | grep -oE '"[0-9]+"' | grep -oE '[0-9]+' || true)"
entries="$(grep -c '^  - constant: ' "$root/github-action-pins.yml" || true)"
if [[ -z "$budget" || -z "$delay" || "$entries" -eq 0 ]]; then
  echo "the retry budget, delay, or entry count could not be read to hold them to each other" >&2
  failures=$((failures + 1))
else
  worst_case_minutes=$(((entries * ceiling * delay + 59) / 60))
  if [[ "$budget" -le "$worst_case_minutes" ]]; then
    echo "the workflow allows $budget minutes but $entries repositories retrying $ceiling times at ${delay}s costs $worst_case_minutes" >&2
    failures=$((failures + 1))
  fi
fi

if [[ "$failures" -ne 0 ]]; then
  echo "$failures of $case_number Action-pin evidence cases did not behave as stated." >&2
  exit 1
fi

echo "The Action-pin check is complete and resilient as stated ($case_number cases)."
