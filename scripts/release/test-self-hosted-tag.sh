#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT

plan_path="$temporary/release-plan.json"
printf '{"digest":"sha256:test","contract":"contract-1","generator":{"tool":"intentional","version":"0.1.5"},"release_units":[],"tags":[],"tag_order":[]}\n' \
  >"$plan_path"

workspace_version="$(python3 "$root/scripts/release/verify-versions.py" | awk '{print $NF}')"

stub_binary="$temporary/intentional"
cat >"$stub_binary" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [[ "\${1:-}" == "--version" ]]; then
  echo "intentional $workspace_version"
  exit 0
fi
printf '%s\n' "\$*" > "\${INTENTIONAL_STUB_LOG:?INTENTIONAL_STUB_LOG is required}"
EOF
chmod +x "$stub_binary"

export SELF_HOSTED_TAG_BINARY_OVERRIDE="$stub_binary"
export SELF_HOSTED_TAG_SKIP_BUILD=1
export SELF_HOSTED_TAG_PRINT_COMMAND=1

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "self-hosted tag test failed for $label: expected output to contain '$needle'" >&2
    echo "$haystack" >&2
    exit 1
  fi
  echo "self-hosted tag command verified for $label"
}

assert_missing() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "self-hosted tag test failed for $label: output must not contain '$needle'" >&2
    echo "$haystack" >&2
    exit 1
  fi
  echo "self-hosted tag command verified for $label (absent: $needle)"
}

verify_output="$("$root/scripts/release/verify-self-hosted-tag.sh" "$plan_path")"
assert_contains "dry-run verification" "$verify_output" "mode=dry-run"
assert_contains "dry-run flag" "$verify_output" "--dry-run"

create_output="$(
  INTENTIONAL_SELF_RELEASE_CREATE=create-annotated-tag \
    "$root/scripts/release/create-self-hosted-tag.sh" "$plan_path"
)"
assert_contains "guarded create mode" "$create_output" "mode=create"
assert_missing "create without dry-run" "$create_output" "--dry-run"

if "$root/scripts/release/create-self-hosted-tag.sh" "$plan_path" >/dev/null 2>&1; then
  echo "self-hosted tag test failed: create path must refuse without CREATE ack" >&2
  exit 1
fi
echo "self-hosted tag create path refuses without explicit operator ack"

export INTENTIONAL_STUB_LOG="$temporary/dry-run-invocation.log"
unset SELF_HOSTED_TAG_PRINT_COMMAND
"$root/scripts/release/verify-self-hosted-tag.sh" "$plan_path" >/dev/null
if ! grep -Fq -- '--dry-run' "$temporary/dry-run-invocation.log"; then
  echo "self-hosted tag test failed: verification must invoke materialized binary with --dry-run" >&2
  cat "$temporary/dry-run-invocation.log" >&2
  exit 1
fi
echo "self-hosted tag verification invokes materialized binary with --dry-run"

echo "self-hosted tag command selection tests passed"
