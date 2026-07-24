#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---
# shellcheck shell=bash

# Shared helpers for self-hosted release tagging with the materialized workspace
# binary. Source this file; do not execute it directly.

self_hosted_tag_root() {
  cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd
}

resolve_self_hosted_plan_path() {
  local plan_path="${1:-${INTENTIONAL_SEALED_PLAN:-}}"
  if [[ -z "$plan_path" ]]; then
    echo "a sealed plan path argument or INTENTIONAL_SEALED_PLAN is required" >&2
    return 1
  fi
  if [[ ! -f "$plan_path" ]]; then
    echo "sealed release plan not found: $plan_path" >&2
    return 1
  fi
  printf '%s' "$plan_path"
}

reject_self_hosted_create_test_seams() {
  if [[ -n "${SELF_HOSTED_TAG_BINARY_OVERRIDE:-}" ]]; then
    echo "refusing real self-hosted tag creation with SELF_HOSTED_TAG_BINARY_OVERRIDE set" >&2
    return 1
  fi
  if [[ "${SELF_HOSTED_TAG_SKIP_BUILD:-}" == "1" ]]; then
    echo "refusing real self-hosted tag creation with SELF_HOSTED_TAG_SKIP_BUILD=1" >&2
    return 1
  fi
  if [[ "${SELF_HOSTED_TAG_PRINT_COMMAND:-}" == "1" ]]; then
    echo "refusing real self-hosted tag creation with SELF_HOSTED_TAG_PRINT_COMMAND=1" >&2
    return 1
  fi
}

materialize_workspace_binary_for_verify() {
  local root="$1"
  if [[ -n "${SELF_HOSTED_TAG_BINARY_OVERRIDE:-}" ]]; then
    if [[ ! -x "$SELF_HOSTED_TAG_BINARY_OVERRIDE" ]]; then
      echo "SELF_HOSTED_TAG_BINARY_OVERRIDE is not executable: $SELF_HOSTED_TAG_BINARY_OVERRIDE" >&2
      return 1
    fi
    printf '%s' "$SELF_HOSTED_TAG_BINARY_OVERRIDE"
    return 0
  fi
  if [[ "${SELF_HOSTED_TAG_SKIP_BUILD:-}" != "1" ]]; then
    cargo build --release --locked -p intentional-cli --manifest-path "$root/Cargo.toml"
  fi
  local binary="$root/target/release/intentional"
  if [[ ! -x "$binary" ]]; then
    echo "materialized workspace binary is missing: $binary" >&2
    return 1
  fi
  printf '%s' "$binary"
}

materialize_workspace_binary_for_create() {
  local root="$1"
  reject_self_hosted_create_test_seams
  cargo build --release --locked -p intentional-cli --manifest-path "$root/Cargo.toml"
  local binary="$root/target/release/intentional"
  if [[ ! -x "$binary" ]]; then
    echo "materialized workspace binary is missing: $binary" >&2
    return 1
  fi
  if [[ "$binary" != "$root/target/release/intentional" ]]; then
    echo "refusing real self-hosted tag creation without workspace target/release/intentional" >&2
    return 1
  fi
  printf '%s' "$binary"
}

verify_materialized_binary_version() {
  local root="$1"
  local binary="$2"
  local workspace_version binary_version
  workspace_version="$(python3 "$root/scripts/release/verify-versions.py" | awk '{print $NF}')"
  binary_version="$("$binary" --version | awk '{print $NF}')"
  if [[ "$binary_version" != "$workspace_version" ]]; then
    echo "materialized binary version $binary_version does not match release projections $workspace_version" >&2
    return 1
  fi
  printf '%s' "$workspace_version"
}

# Modes: dry-run (default verification) or create (real annotated tag creation).
self_hosted_tag_argv() {
  local mode="$1"
  local root="$2"
  local binary="$3"
  local plan_path="$4"
  local -a argv=("$binary" "-C" "$root" "tag" "--plan" "$plan_path")
  case "$mode" in
    dry-run)
      argv+=(--dry-run)
      ;;
    create) ;;
    *)
      echo "unknown self-hosted tag mode: $mode" >&2
      return 1
      ;;
  esac
  local entry
  for entry in "${argv[@]}"; do
    printf '%s\0' "$entry"
  done
}

print_self_hosted_tag_command() {
  local mode="$1"
  local root="$2"
  local binary="$3"
  local plan_path="$4"
  local -a argv=()
  local entry
  while IFS= read -r -d '' entry; do
    argv+=("$entry")
  done < <(self_hosted_tag_argv "$mode" "$root" "$binary" "$plan_path")
  printf 'mode=%s\n' "$mode"
  printf 'command=%q' "${argv[0]}"
  local index
  for ((index = 1; index < ${#argv[@]}; index++)); do
    printf ' %q' "${argv[$index]}"
  done
  printf '\n'
}

require_self_hosted_create_ack() {
  if [[ "${INTENTIONAL_SELF_RELEASE_CREATE:-}" != "create-annotated-tag" ]]; then
    echo "refusing real self-hosted tag creation without INTENTIONAL_SELF_RELEASE_CREATE=create-annotated-tag" >&2
    echo "use task self-release:tag with CREATE_ACK=create-annotated-tag for the guarded operator path" >&2
    return 1
  fi
}

run_self_hosted_tag() {
  local mode="$1"
  local root="$2"
  local plan_path="$3"
  local binary version
  plan_path="$(resolve_self_hosted_plan_path "$plan_path")"
  if [[ "$mode" == "create" ]]; then
    require_self_hosted_create_ack
    reject_self_hosted_create_test_seams
    binary="$(materialize_workspace_binary_for_create "$root")"
  else
    binary="$(materialize_workspace_binary_for_verify "$root")"
    if [[ "${SELF_HOSTED_TAG_PRINT_COMMAND:-}" == "1" ]]; then
      version="$(verify_materialized_binary_version "$root" "$binary")"
      echo "materialized workspace binary agrees at $version"
      print_self_hosted_tag_command "$mode" "$root" "$binary" "$plan_path"
      return 0
    fi
  fi
  version="$(verify_materialized_binary_version "$root" "$binary")"
  echo "materialized workspace binary agrees at $version"
  local -a argv=()
  local entry
  while IFS= read -r -d '' entry; do
    argv+=("$entry")
  done < <(self_hosted_tag_argv "$mode" "$root" "$binary" "$plan_path")
  "${argv[@]}"
  if [[ "$mode" == "dry-run" ]]; then
    echo "self-hosted tag dry-run accepted sealed plan from $plan_path"
  else
    echo "self-hosted tag creation completed with materialized binary from $plan_path"
  fi
}
