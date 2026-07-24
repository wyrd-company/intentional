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

git -C "$workspace" init -b main
git -C "$workspace" config user.email "fixture@example.invalid"
git -C "$workspace" config user.name "Garden Notes Fixture"
git -C "$workspace" add .
git -C "$workspace" commit -m "Initialize garden-notes fixture"
git -C "$workspace" check-ignore -q node_modules || true

home_dir="$workspace/.home"
mkdir -p "$home_dir"
chmod 755 "$workspace"
chmod -R u=rwX,go=rX "$workspace"
chmod 755 "$workspace/intentional"

user_id="$(id -u)"
group_id="$(id -g)"
smoke_log="$workspace/smoke.log"

set +e
docker run --rm \
  --user "${user_id}:${group_id}" \
  -v "$workspace:/smoke:rw" \
  -w /smoke \
  -e HOME=/smoke/.home \
  -e PATH=/smoke:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  "$runtime_image" \
  bash -euxo pipefail -c '
    cd /smoke
    test -d .git
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
    intentional tag --baseline --version garden-notes=1.0.0
    intentional status
    intentional check
    intentional plan
  ' >"$smoke_log" 2>&1
smoke_status=$?
set -e

cat "$smoke_log"
if [[ "$smoke_status" -ne 0 ]]; then
  echo "linux-gnu smoke failed for $label with exit $smoke_status" >&2
  exit "$smoke_status"
fi

echo "linux-gnu smoke passed for $label"
