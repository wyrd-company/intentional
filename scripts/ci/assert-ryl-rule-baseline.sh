#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
configuration="${1:-$root/.ryl.toml}"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

for command in ryl yq; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "the RYL rule baseline assertion requires $command, but it is unavailable" >&2
    exit 1
  fi
done

# The literal JSON Schema key is named "$defs"; this is not shell expansion.
# shellcheck disable=SC2016
ryl --print-toml-config-schema |
  yq -r '."$defs".RulesTable.properties | keys | .[]' |
  sort -u >"$temporary/shipped"
awk '
  /^\[rules\.[a-z0-9-]+\]$/ {
    rule = $0
    sub(/^\[rules\./, "", rule)
    sub(/\]$/, "", rule)
    print rule
  }
' "$configuration" | sort -u >"$temporary/enabled"
awk '
  /^\[rules\.[a-z0-9-]+\]$/ {
    rule = $0
    sub(/^\[rules\./, "", rule)
    sub(/\]$/, "", rule)
    next
  }
  /^\[/ { rule = "" }
  rule != "" {
    setting = $0
    sub(/#.*/, "", setting)
    gsub(/[[:space:]]/, "", setting)
    gsub(/"/, "", setting)
    gsub(sprintf("%c", 39), "", setting)
    if (setting == "level=disabled" || setting == "level=disable" ||
        setting == "enabled=false" || setting == "enabled=disabled" ||
        setting == "enabled=disable") {
      print rule
    }
  }
' "$configuration" | sort -u >"$temporary/disabled"
awk '
  /^# excluded-rule: [a-z0-9-]+ - .+/ {
    rule = $3
    print rule
  }
' "$configuration" | sort -u >"$temporary/excluded"

disabled="$(paste -sd, "$temporary/disabled")"
if [[ -n "$disabled" ]]; then
  echo "RYL rules disabled in configuration require exclusion rationale: $disabled" >&2
  exit 1
fi

overlap="$(comm -12 "$temporary/enabled" "$temporary/excluded" | paste -sd, -)"
if [[ -n "$overlap" ]]; then
  echo "RYL rules cannot be both enabled and excluded: $overlap" >&2
  exit 1
fi

sort -u "$temporary/enabled" "$temporary/excluded" >"$temporary/classified"
missing="$(comm -23 "$temporary/shipped" "$temporary/classified" | paste -sd, -)"
unknown="$(comm -13 "$temporary/shipped" "$temporary/classified" | paste -sd, -)"
if [[ -n "$missing" ]]; then
  echo "unclassified shipped RYL rules: $missing" >&2
  exit 1
fi
if [[ -n "$unknown" ]]; then
  echo "the RYL baseline classifies rules not shipped by this version: $unknown" >&2
  exit 1
fi

echo "Every rule shipped by $(ryl --version) is explicitly enabled or excluded."
