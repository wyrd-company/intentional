#!/usr/bin/env bash
# ---

# Assignments feed helpers defined by the sourcing dispatcher.
# shellcheck disable=SC2034
# relationships:
#   implements: github-release-executor
# ---

INTENTIONAL_REGISTRY=$INPUT_REGISTRY
INTENTIONAL_SCOPE=$INPUT_SCOPE

ALLOWED=("PATH=${PATH:-}" "HOME=${HOME:-}" "RUSTUP_HOME=${RUSTUP_HOME:-}")
SCOPE_ARGUMENTS=()
if [[ -n "$INTENTIONAL_SCOPE" ]]; then
  SCOPE_ARGUMENTS=("--${INTENTIONAL_SCOPE}:registry=${INTENTIONAL_REGISTRY}")
fi
if [[ -n "$INPUT_REGISTRY_TOKEN" ]]; then
  host=${INTENTIONAL_REGISTRY#https://}
  npm config set "//${host%/}/:_authToken=${INPUT_REGISTRY_TOKEN}"
fi

npm_holds() {
  mkdir -p "$INTENTIONAL_WORK/probe"
  if view=$(cd "$INTENTIONAL_WORK/probe" \
    && env -i "${ALLOWED[@]}" npm view "$1" dist.integrity \
      --registry "$INTENTIONAL_REGISTRY" \
      ${SCOPE_ARGUMENTS[@]+"${SCOPE_ARGUMENTS[@]}"} \
      2>"$INTENTIONAL_WORK/npm-error"); then
    printf '%s' "$view"
    return 0
  fi
  case "$(cat "$INTENTIONAL_WORK/npm-error")" in
    *E404*|*"404 Not Found"*) return 1 ;;
    *) cat "$INTENTIONAL_WORK/npm-error" >&2; return 2 ;;
  esac
}

mkdir -p "$INTENTIONAL_WORK"
tarball=$(find "$INTENTIONAL_SUBJECT" -maxdepth 1 -name '*.tgz' -print -quit)
test -n "$tarball"
local_digest="sha512-$(openssl dgst -sha512 -binary "$tarball" | base64 -w0)"
INTENTIONAL_DESTINATION_DIGEST=""
INTENTIONAL_ELAPSED=0
while :; do
  INTENTIONAL_DESTINATION_DIGEST=$(npm_holds \
    "$INTENTIONAL_SUBJECT_IDENTITY@$INTENTIONAL_VERSION" || true)
  if [[ -n "$INTENTIONAL_DESTINATION_DIGEST" ]]; then break; fi
  if ! wait_again; then
    observe_state pending
    exit 0
  fi
done
if [[ "$INTENTIONAL_DESTINATION_DIGEST" != "$local_digest" ]]; then
  observe_state conflict \
    "$INTENTIONAL_SUBJECT_IDENTITY@$INTENTIONAL_VERSION publishes integrity $INTENTIONAL_DESTINATION_DIGEST, not the promoted $local_digest"
  exit 0
fi

rm -rf "$INTENTIONAL_WORK/clean"
mkdir -p "$INTENTIONAL_WORK/clean"
if [[ "$INTENTIONAL_RETRIEVAL_MODE" == public ]]; then
  : > "$INTENTIONAL_WORK/clean/npmrc"
else
  host=${INTENTIONAL_REGISTRY#https://}
  printf '//%s/:_authToken=%s\n' "${host%/}" "$INPUT_REGISTRY_TOKEN" \
    > "$INTENTIONAL_WORK/clean/npmrc"
fi
(
  cd "$INTENTIONAL_WORK/clean" || exit
  npm_config_userconfig="$INTENTIONAL_WORK/clean/npmrc" \
    npm pack "$INTENTIONAL_SUBJECT_IDENTITY@$INTENTIONAL_VERSION" \
      --registry "$INTENTIONAL_REGISTRY" \
      --cache "$INTENTIONAL_WORK/clean/cache" >/dev/null
)
retrieved=$(find "$INTENTIONAL_WORK/clean" -maxdepth 1 -name '*.tgz' -print -quit)
test -n "$retrieved"
INTENTIONAL_RETRIEVED_DIGEST="sha512-$(openssl dgst -sha512 -binary "$retrieved" | base64 -w0)"
test "$INTENTIONAL_RETRIEVED_DIGEST" = "$local_digest"
INTENTIONAL_PACKAGER_VERSION=$(npm --version)
INTENTIONAL_RETRIEVAL_VERSION=$INTENTIONAL_PACKAGER_VERSION
observe_present
