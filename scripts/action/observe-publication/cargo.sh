#!/usr/bin/env bash
# ---

# Assignments feed helpers defined by the sourcing dispatcher.
# shellcheck disable=SC2034
# relationships:
#   implements: github-release-executor
# ---

REGISTRY_ARGUMENTS=()
ALLOWED=("PATH=${PATH:-}" "HOME=${HOME:-}" "RUSTUP_HOME=${RUSTUP_HOME:-}" CARGO_NET_OFFLINE=false)
if [[ -n "$INPUT_REGISTRY_NAME" ]]; then
  REGISTRY_ARGUMENTS=(--registry "$INPUT_REGISTRY_NAME")
  ALLOWED+=("$INPUT_REGISTRY_INDEX_VARIABLE=$INPUT_REGISTRY_INDEX_URL")
fi
if [[ -n "$INPUT_CARRIED_TOKEN" && -n "${!INPUT_CARRIED_TOKEN:-}" ]]; then
  ALLOWED+=("$INPUT_CARRIED_TOKEN=${!INPUT_CARRIED_TOKEN}")
fi

resolve() {
  rm -rf "$1"
  mkdir -p "$1"
  env -i "${ALLOWED[@]}" CARGO_HOME="$1/home" \
    cargo new --quiet --lib "$1/probe" >/dev/null
  if (
    cd "$1/probe"
    env -i "${ALLOWED[@]}" CARGO_HOME="$1/home" \
      cargo add --quiet ${REGISTRY_ARGUMENTS[@]+"${REGISTRY_ARGUMENTS[@]}"} \
        "$INTENTIONAL_SUBJECT_IDENTITY@=$INTENTIONAL_VERSION"
    env -i "${ALLOWED[@]}" CARGO_HOME="$1/home" cargo fetch --quiet
  ) > "$1/log" 2>&1; then
    return 0
  fi
  if grep -qiE 'could not be found|not found in registry|no matching package' "$1/log"; then
    return 1
  fi
  cat "$1/log" >&2
  return 2
}

mkdir -p "$INTENTIONAL_WORK"
crate=$(find "$INTENTIONAL_SUBJECT" -maxdepth 1 -name '*.crate' -print -quit)
test -n "$crate"
local_digest=$(sha256sum < "$crate" | cut -d' ' -f1)
INTENTIONAL_ELAPSED=0
resolved=no
while :; do
  if resolve "$INTENTIONAL_WORK/clean"; then resolved=yes; break; fi
  if ! wait_again; then
    observe_state pending
    exit 0
  fi
done
test "$resolved" = yes
INTENTIONAL_DESTINATION_DIGEST=$(sed -n \
  "/^name = \"$INTENTIONAL_SUBJECT_IDENTITY\"$/,/^$/p" \
  "$INTENTIONAL_WORK/clean/probe/Cargo.lock" \
  | sed -n 's/^checksum = "\(.*\)"$/\1/p')
test -n "$INTENTIONAL_DESTINATION_DIGEST"
retrieved=$(find "$INTENTIONAL_WORK/clean/home/registry/cache" -type f \
  -name "$INTENTIONAL_SUBJECT_IDENTITY-$INTENTIONAL_VERSION.crate" -print -quit)
test -n "$retrieved"
INTENTIONAL_RETRIEVED_DIGEST=$(sha256sum < "$retrieved" | cut -d' ' -f1)
if [[ "$INTENTIONAL_DESTINATION_DIGEST" != "$local_digest" ]]; then
  observe_state conflict \
    "$INTENTIONAL_SUBJECT_IDENTITY $INTENTIONAL_VERSION publishes checksum $INTENTIONAL_DESTINATION_DIGEST, not the promoted $local_digest"
  exit 0
fi
test "$INTENTIONAL_RETRIEVED_DIGEST" = "$local_digest"
INTENTIONAL_PACKAGER_VERSION=$(cargo --version | cut -d' ' -f2)
INTENTIONAL_RETRIEVAL_VERSION=$INTENTIONAL_PACKAGER_VERSION
observe_present
