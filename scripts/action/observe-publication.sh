#!/usr/bin/env bash
# ---
# relationships:
#   implements: github-release-executor
# ---

# Values are consumed by the sourced adapter selected at the end of this file.
# GITHUB_ACTION_PATH fixes the source paths, but ShellCheck cannot resolve it.
# shellcheck disable=SC2034,SC1091
set -euo pipefail

INTENTIONAL_RELEASE_UNIT=${INTENTIONAL_RELEASE_UNIT:-$INPUT_RELEASE_UNIT}
INTENTIONAL_PACKAGE=${INTENTIONAL_PACKAGE:-$INPUT_PACKAGE}
INTENTIONAL_PUBLISHER=${INTENTIONAL_PUBLISHER:-$INPUT_PUBLISHER}
INTENTIONAL_TARGET=${INTENTIONAL_TARGET:-$INPUT_TARGET}
INTENTIONAL_OBSERVATION=${INTENTIONAL_OBSERVATION:-$INPUT_OBSERVATION}
INTENTIONAL_SUBJECT=${INTENTIONAL_SUBJECT:-$INPUT_SUBJECT}
INTENTIONAL_SUBJECT_KIND=${INTENTIONAL_SUBJECT_KIND:-$INPUT_SUBJECT_KIND}
INTENTIONAL_SUBJECT_IDENTITY=${INTENTIONAL_SUBJECT_IDENTITY:-$INPUT_SUBJECT_IDENTITY}
INTENTIONAL_VERSION=${INTENTIONAL_VERSION:-$INPUT_SUBJECT_VERSION}
INTENTIONAL_SUBJECT_DIGEST=${INTENTIONAL_SUBJECT_DIGEST:-$INPUT_SUBJECT_DIGEST}
INTENTIONAL_PACKAGER_ID=${INTENTIONAL_PACKAGER_ID:-$INPUT_PACKAGER}
INTENTIONAL_DESTINATION=${INTENTIONAL_DESTINATION:-$INPUT_DESTINATION}
INTENTIONAL_RETRIEVAL_MODE=${INTENTIONAL_RETRIEVAL_MODE:-$INPUT_RETRIEVAL_MODE}
INTENTIONAL_RETRIEVAL_CLIENT=${INTENTIONAL_RETRIEVAL_CLIENT:-$INPUT_RETRIEVAL_CLIENT}
INTENTIONAL_WORK=${INTENTIONAL_WORK:-$INPUT_WORK}
INTENTIONAL_INTERVAL=${INTENTIONAL_INTERVAL:-$INPUT_INTERVAL}
INTENTIONAL_BACKOFF=${INTENTIONAL_BACKOFF:-$INPUT_BACKOFF}
INTENTIONAL_MAXIMUM_INTERVAL=${INTENTIONAL_MAXIMUM_INTERVAL:-$INPUT_MAXIMUM_INTERVAL}
INTENTIONAL_DEADLINE=${INTENTIONAL_DEADLINE:-$INPUT_DEADLINE}

observe_header() {
  # $schema is a literal YAML key, not a shell expansion.
  # shellcheck disable=SC2016
  printf '$schema: https://intentional.foo/schemas/publication-observation/v1\n'
  printf 'contract: publication-observation-1\n'
  printf 'release-unit: "%s"\n' "$INTENTIONAL_RELEASE_UNIT"
  printf 'package: "%s"\n' "$INTENTIONAL_PACKAGE"
  printf 'publisher: "%s"\n' "$INTENTIONAL_PUBLISHER"
  printf 'target: "%s"\n' "$INTENTIONAL_TARGET"
}

observe_state() {
  mkdir -p "$(dirname "$INTENTIONAL_OBSERVATION")"
  {
    observe_header
    printf 'state: %s\n' "$1"
    if [[ "$1" == conflict ]]; then printf 'conflict: "%s"\n' "$2"; fi
  } > "$INTENTIONAL_OBSERVATION"
}

observe_present() {
  mkdir -p "$(dirname "$INTENTIONAL_OBSERVATION")"
  {
    observe_header
    printf 'state: present\n'
    printf 'subject:\n'
    printf '  kind: "%s"\n' "$INTENTIONAL_SUBJECT_KIND"
    printf '  identity: "%s"\n' "$INTENTIONAL_SUBJECT_IDENTITY"
    printf '  version: "%s"\n' "$INTENTIONAL_VERSION"
    printf '  digest: "%s"\n' "$INTENTIONAL_SUBJECT_DIGEST"
    printf 'packager:\n'
    printf '  id: "%s"\n' "$INTENTIONAL_PACKAGER_ID"
    printf '  version: "%s"\n' "$INTENTIONAL_PACKAGER_VERSION"
    printf 'destination:\n'
    printf '  identity: "%s"\n' "$INTENTIONAL_DESTINATION"
    printf '  version: "%s"\n' "$INTENTIONAL_VERSION"
    printf '  digest: "%s"\n' "$INTENTIONAL_DESTINATION_DIGEST"
    printf 'retrieval:\n'
    printf '  mode: %s\n' "$INTENTIONAL_RETRIEVAL_MODE"
    printf '  client: "%s"\n' "$INTENTIONAL_RETRIEVAL_CLIENT"
    printf '  version: "%s"\n' "$INTENTIONAL_RETRIEVAL_VERSION"
    printf '  digest: "%s"\n' "$INTENTIONAL_RETRIEVED_DIGEST"
  } > "$INTENTIONAL_OBSERVATION"
}

wait_again() {
  if [[ "$INTENTIONAL_ELAPSED" -ge "$INTENTIONAL_DEADLINE" ]]; then return 1; fi
  local wait=$INTENTIONAL_INTERVAL
  local remaining=$((INTENTIONAL_DEADLINE - INTENTIONAL_ELAPSED))
  if [[ "$wait" -gt "$remaining" ]]; then wait=$remaining; fi
  sleep "$wait"
  INTENTIONAL_ELAPSED=$((INTENTIONAL_ELAPSED + wait))
  INTENTIONAL_INTERVAL=$((INTENTIONAL_INTERVAL * INTENTIONAL_BACKOFF))
  if [[ "$INTENTIONAL_INTERVAL" -gt "$INTENTIONAL_MAXIMUM_INTERVAL" ]]; then
    INTENTIONAL_INTERVAL=$INTENTIONAL_MAXIMUM_INTERVAL
  fi
}

case "$INTENTIONAL_PUBLISHER" in
  npm) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/npm.sh" ;;
  cargo) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/cargo.sh" ;;
  homebrew|aur) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/repository.sh" ;;
  rpm|apt) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/system-package.sh" ;;
  oci) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/oci.sh" ;;
  *) printf 'unsupported publication observer: %s\n' "$INTENTIONAL_PUBLISHER" >&2; exit 1 ;;
esac
