#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

hosted_task_calls="$(
  yq -r '[.jobs[] | .steps[]? | select(.run == "task ci")] | length' \
    "$root/.github/workflows/ci.yml"
)"
if [[ "$hosted_task_calls" -ne 1 ]]; then
  echo "hosted CI must invoke task ci exactly once; found $hosted_task_calls calls" >&2
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

echo "Hosted CI reuses task ci, and shell:lint reaches every shell script."
