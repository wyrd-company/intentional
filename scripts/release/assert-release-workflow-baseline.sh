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

if grep -En 'cargo build --workspace --release.*x86_64-unknown-linux-gnu' "$workflow" >/dev/null; then
  echo "Release workflow must not build x86_64-unknown-linux-gnu with host cargo." >&2
  exit 1
fi

if ! grep -Fq 'cross build --workspace --release --locked --target' "$workflow"; then
  echo "Release workflow must build Linux GNU targets through cross." >&2
  exit 1
fi

if ! grep -Fq 'x86_64-unknown-linux-gnu' "$workflow" || ! grep -Fq 'aarch64-unknown-linux-gnu' "$workflow"; then
  echo "Release workflow must reference both Linux GNU targets." >&2
  exit 1
fi

if ! grep -Fq 'cross@0.2.5' "$workflow"; then
  echo "Release workflow must install cross 0.2.5 for Linux GNU builds." >&2
  exit 1
fi

if ! grep -Fq "hashFiles('Cross.toml'" "$workflow" || \
   ! grep -Fq 'scripts/release/linux-gnu-baseline.env' "$workflow"; then
  echo "Release workflow cache key must include the Linux GNU baseline inputs." >&2
  exit 1
fi

for image in "$X86_64_GNU_CROSS_IMAGE" "$AARCH64_GNU_CROSS_IMAGE"; do
  if ! grep -Fq "$image" "$cross_toml"; then
    echo "Cross.toml is missing pinned image: $image" >&2
    exit 1
  fi
done

if grep -En 'ghcr\.io/cross-rs/[^:]+:(main|latest)' "$cross_toml" >/dev/null; then
  echo "Cross.toml must not reference mutable cross image tags." >&2
  exit 1
fi

echo "Release workflow preserves the pinned Linux GNU baseline."
