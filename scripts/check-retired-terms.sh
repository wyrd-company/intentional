#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---
#
# Fails when a retired name for the annotated tag message returns. The tag
# message is the tag record; a release record is the GitHub Release published
# per Release Unit. The fixed header `intentional release record` stays allowed.

set -euo pipefail

cd "$(dirname "$0")/.."

# Each rule is `id|pattern`. "release record" names the tag message only when
# tag wording qualifies it; unqualified uses ("release-record policy", "a
# release record on its primary tag, such as a GitHub Release") keep the
# per-unit GitHub Release meaning.
rules=(
  'intentional-record|\bintentional\s+records?\b(?!\s+(this|that|the|a|an|its|it|each|every|which|whether)\b)'
  'protocol-record|\bprotocol\s+records?\b'
  'baseline-record|\bbaseline\s+records?\b'
  'tag-qualified-release-record|(?<!intentional )\b(annotated|tag|tag-message|workspace)\s+release(\s+|-)records?\b'
  'release-record-fields|(?<!intentional )\brelease(\s+|-)records?(\x27s)?[ \t]+`?(fields?|header|message|codec|tag[ -]id|plan-digest|contract|baseline)\b'
  'release-record-of-tag|(?<!intentional )\brelease(\s+|-)records?\s+(in|of)\s+(an?\s+|the\s+|each\s+|every\s+)?(annotated\s+)?tags?\b'
  'release-record-near-tag-message|\btag\s+messages?\b[^.]{0,120}(?<!intentional )\brelease(\s+|-)records?\b|(?<!intentional )\brelease(\s+|-)records?\b[^.]{0,120}\btag\s+messages?\b'
)

scan_roots=(docs crates .intentional/intents npm README.md CONTRIBUTING.md AGENTS.md action.yml)
fixtures=scripts/fixtures/retired-terms

# scan RULE PATH... prints every match of one rule. Exit 0: no match,
# 1: match, 2: search error.
scan() {
  local rule="$1" output status
  shift
  if output="$(rg --pcre2 --multiline --ignore-case --line-number --no-heading \
    --with-filename --glob '!**/amber-river-3568.md' -e "${rule#*|}" "$@")"; then
    printf '%s\n' "$output" | sed "s/^/retired term (${rule%%|*}): /"
    return 1
  else
    status=$?
  fi
  test "$status" -eq 1 && return 0
  echo "retired-term search failed for rule ${rule%%|*}" >&2
  return 2
}

for root in "${scan_roots[@]}"; do
  test -e "$root" || { echo "retired-term scan root missing: $root" >&2; exit 1; }
done

# Self-test: every rule flags its own fixture and passes every allowed fixture.
for rule in "${rules[@]}"; do
  fixture="$fixtures/flagged/${rule%%|*}.md"
  test -f "$fixture" || { echo "retired-term rule has no fixture: $fixture" >&2; exit 1; }
  status=0
  scan "$rule" "$fixture" >/dev/null || status=$?
  test "$status" -eq 1 || { echo "retired-term rule ${rule%%|*} missed $fixture" >&2; exit 1; }
  scan "$rule" "$fixtures/allowed" || { echo "retired-term rule ${rule%%|*} flagged an allowed fixture" >&2; exit 1; }
done

found=0
for rule in "${rules[@]}"; do
  status=0
  scan "$rule" "${scan_roots[@]}" || status=$?
  case "$status" in
    0) ;;
    1) found=1 ;;
    *) exit 1 ;;
  esac
done
if test "$found" -ne 0; then
  echo "Retired tag-message term found; the tag message is the tag record." >&2
  exit 1
fi
