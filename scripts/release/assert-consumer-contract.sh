#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

expected_assets=(
  intentional-linux-x86_64.tar.gz
  intentional-linux-arm64.tar.gz
  intentional-macos-arm64.tar.gz
  intentional-windows-x86_64.zip
  SHA256SUMS
)

for asset in "${expected_assets[@]}"; do
  if ! grep -Fq "$asset" "$root/scripts/release/ensure-github-release.sh"; then
    echo "ensure-github-release.sh is missing asset $asset" >&2
    exit 1
  fi
done

for asset in intentional-linux-x86_64.tar.gz intentional-linux-arm64.tar.gz; do
  if ! grep -Fq "$asset" "$root/npm/install.js"; then
    echo "npm/install.js is missing asset $asset" >&2
    exit 1
  fi
  if ! grep -Fq "$asset" "$root/action.yml"; then
    echo "action.yml is missing asset $asset" >&2
    exit 1
  fi
  if ! grep -Fq "$asset" "$root/scripts/release/render-homebrew-formula.sh"; then
    echo "render-homebrew-formula.sh is missing asset $asset" >&2
    exit 1
  fi
  if ! grep -Fq "$asset" "$root/docs/install.md"; then
    echo "docs/install.md is missing asset $asset" >&2
    exit 1
  fi
done

if ! grep -Fq 'f"{artifact}/"' "$root/scripts/release/package-archive.py"; then
  echo "package-archive.py must preserve artifact directory layout." >&2
  exit 1
fi

echo "Release consumer contract asset names are unchanged."
