#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
#   references: build-linux-gnu-release-on-pinned-cross-baseline
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$root/.github/workflows/cd.yml"
cross_toml="$root/Cross.toml"
baseline_env="$root/scripts/release/linux-gnu-baseline.env"

for file in "$workflow" "$cross_toml" "$baseline_env"; do
  if [[ ! -f "$file" ]]; then
    echo "Missing release baseline file: $file" >&2
    exit 1
  fi
done

# shellcheck disable=SC1090
source "$baseline_env"

if rg -n 'cargo build --workspace --release.*x86_64-unknown-linux-gnu' "$workflow"; then
  echo "Release workflow must not build x86_64-unknown-linux-gnu with host cargo." >&2
  exit 1
fi

if ! rg -q 'cross build --workspace --release --locked --target "\$\{\{ matrix\.target \}\}"' "$workflow"; then
  echo "Release workflow must build Linux GNU targets through cross." >&2
  exit 1
fi

if ! rg -q 'x86_64-unknown-linux-gnu' "$workflow" || ! rg -q 'aarch64-unknown-linux-gnu' "$workflow"; then
  echo "Release workflow must reference both Linux GNU targets." >&2
  exit 1
fi

if ! rg -q 'cross@0\.2\.5' "$workflow"; then
  echo "Release workflow must install cross 0.2.5 for Linux GNU builds." >&2
  exit 1
fi

if ! rg -q "hashFiles\\('Cross\\.toml'" "$workflow" || \
   ! rg -q "scripts/release/linux-gnu-baseline\\.env" "$workflow"; then
  echo "Release workflow cache key must include the Linux GNU baseline inputs." >&2
  exit 1
fi

for image in "$X86_64_GNU_CROSS_IMAGE" "$AARCH64_GNU_CROSS_IMAGE"; do
  if ! rg -Fq "$image" "$cross_toml"; then
    echo "Cross.toml is missing pinned image: $image" >&2
    exit 1
  fi
done

if rg -n 'ghcr\.io/cross-rs/[^:]+:(main|latest)' "$cross_toml"; then
  echo "Cross.toml must not reference mutable cross image tags." >&2
  exit 1
fi

echo "Release workflow preserves the pinned Linux GNU baseline."
