#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/release/self-hosted-tag-lib.sh
source "$root/scripts/release/self-hosted-tag-lib.sh"

run_self_hosted_tag create "$root" "${1:-}"
