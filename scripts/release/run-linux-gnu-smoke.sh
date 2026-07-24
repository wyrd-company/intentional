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
install -m 0755 "$binary" "$workspace/intentional"

user_id="$(id -u)"
group_id="$(id -g)"
smoke_log="$workspace/smoke.log"

set +e
docker run --rm \
  -v "$workspace:/workspace" \
  -w /workspace \
  -e USER_ID="$user_id" \
  -e GROUP_ID="$group_id" \
  "$runtime_image" \
  bash -euxo pipefail -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq git >/dev/null
    export HOME=/workspace/.home
    mkdir -p "$HOME"
    export PATH="/workspace:$PATH"
    git config --global --add safe.directory /workspace
    git init -b main
    git config user.email "fixture@example.invalid"
    git config user.name "Garden Notes Fixture"
    git add .
    git commit -m "Initialize garden-notes fixture"
    intentional --version
    set +e
    intentional init
    init_status=$?
    set -e
    if [[ "$init_status" -eq 2 ]]; then
      test -s .intentional/init-plan.yml
    elif [[ "$init_status" -ne 0 ]]; then
      exit "$init_status"
    fi
    git check-ignore -q node_modules || true
    intentional tag --baseline --version garden-notes=1.0.0
    intentional status
    intentional check
    intentional plan
    chown -R "$USER_ID:$GROUP_ID" /workspace
  ' >"$smoke_log" 2>&1
smoke_status=$?
set -e

cat "$smoke_log"
if [[ "$smoke_status" -ne 0 ]]; then
  echo "linux-gnu smoke failed for $label with exit $smoke_status" >&2
  exit "$smoke_status"
fi

echo "linux-gnu smoke passed for $label"
