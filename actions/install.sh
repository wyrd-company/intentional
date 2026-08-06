#!/usr/bin/env bash
# ---
# relationships:
#   implements: github-release-executor
# ---
#
# Preserve the path used by Actions nested beneath actions/ while delegating
# installation to the repository's one runner-installer implementation.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/../scripts/action/install-intentional.sh" "$@"
