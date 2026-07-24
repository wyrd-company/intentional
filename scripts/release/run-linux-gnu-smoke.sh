#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture="$root/scripts/release/fixtures/garden-notes"
binary="${1:?release binary path is required}"
runtime_image="${2:?runtime image digest is required}"
label="${3:-$runtime_image}"

if [[ ! -f "$binary" ]]; then
  echo "Smoke binary does not exist: $binary" >&2
  exit 1
fi

workspace="$(mktemp -d)"
trap 'rm -rf "$workspace"' EXIT

cp -a "$fixture/." "$workspace/"
chmod +x "$binary"
install -m 0755 "$binary" "$workspace/intentional"

docker run --rm \
  -v "$workspace:/workspace" \
  -w /workspace \
  "$runtime_image" \
  bash -euxo pipefail -c '
    export PATH="/workspace:$PATH"
    git init
    git config user.email "fixture@example.invalid"
    git config user.name "Garden Notes Fixture"
    git add .
    git commit -m "Initialize garden-notes fixture"
    intentional --version
    intentional init
    git check-ignore -q node_modules || true
    intentional tag --baseline --version garden-notes=1.0.0
    intentional status
    intentional check
    intentional plan
  ' >"$workspace/smoke.log" 2>&1

cp "$workspace/smoke.log" "/tmp/linux-gnu-smoke-${label//[^a-zA-Z0-9_.-]/_}.log" 2>/dev/null || true
cat "$workspace/smoke.log"
echo "linux-gnu smoke passed for $label"
