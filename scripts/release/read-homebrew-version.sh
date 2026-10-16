#!/usr/bin/env bash
# ---
# relationships:
#   references: intent-driven-polyglot-release
# ---
set -euo pipefail

formula="${1:?formula path is required}"
sed -nE 's|.*releases/download/([^/]+)/.*|\1|p' "$formula" | sort -u
