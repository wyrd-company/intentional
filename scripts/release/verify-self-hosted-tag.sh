#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
plan_path="${1:-${INTENTIONAL_SEALED_PLAN:-}}"

if [[ -z "$plan_path" ]]; then
  echo "verify-self-hosted-tag.sh requires a sealed plan path argument or INTENTIONAL_SEALED_PLAN" >&2
  exit 1
fi

if [[ ! -f "$plan_path" ]]; then
  echo "sealed release plan not found: $plan_path" >&2
  exit 1
fi

cargo build --release --locked -p intentional-cli --manifest-path "$root/Cargo.toml"
binary="$root/target/release/intentional"

workspace_version="$(python3 "$root/scripts/release/verify-versions.py" | awk '{print $NF}')"
binary_version="$("$binary" --version | awk '{print $NF}')"

if [[ "$binary_version" != "$workspace_version" ]]; then
  echo "materialized binary version $binary_version does not match release projections $workspace_version" >&2
  exit 1
fi

echo "materialized workspace binary agrees at $workspace_version"
"$binary" -C "$root" tag --plan "$plan_path" --dry-run
echo "self-hosted tag dry-run accepted sealed plan from $plan_path"
