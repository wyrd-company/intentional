#!/usr/bin/env bash
# ---
# relationships:
#   implements: github-release-executor
# ---

# GITHUB_ACTION_PATH fixes the source paths, but ShellCheck cannot resolve them.
# shellcheck disable=SC1091
set -euo pipefail

source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/common.sh"

case "$INTENTIONAL_PUBLISHER" in
  npm) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/npm.sh" ;;
  cargo) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/cargo.sh" ;;
  homebrew|aur) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/repository.sh" ;;
  rpm|apt) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/system-package.sh" ;;
  oci) source "$GITHUB_ACTION_PATH/../../scripts/action/observe-publication/oci.sh" ;;
  *) printf 'unsupported publication observer: %s\n' "$INTENTIONAL_PUBLISHER" >&2; exit 1 ;;
esac
