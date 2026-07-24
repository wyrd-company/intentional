#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
#   references: build-linux-gnu-release-on-pinned-cross-baseline
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
baseline_env="$root/scripts/release/linux-gnu-baseline.env"

if [[ ! -f "$baseline_env" ]]; then
  echo "Missing Linux GNU baseline file: $baseline_env" >&2
  exit 1
fi

# shellcheck disable=SC1090
source "$baseline_env"

binary="${1:?release GNU binary path is required}"
label="${2:-$binary}"

if [[ ! -f "$binary" ]]; then
  echo "Release GNU binary does not exist: $binary" >&2
  exit 1
fi

for tool in file readelf; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Required inspection tool is unavailable: $tool" >&2
    exit 1
  fi
done

echo "label=$label"
file "$binary"
readelf -l "$binary" | sed -n '/INTERP/,+1p'
readelf -d "$binary" | sed -n '/NEEDED/p' || true

version_symbols="$(readelf -V "$binary")"
printf '%s\n' "$version_symbols"

max_glibc="$(
  printf '%s\n' "$version_symbols" |
    grep -oE 'GLIBC_[0-9]+(\.[0-9]+)*' |
    sort -V |
    tail -1
)"

if [[ -z "$max_glibc" ]]; then
  echo "No GLIBC versioned symbols found in $binary." >&2
  exit 1
fi

echo "max_glibc_symbol=$max_glibc"
echo "allowed_max_glibc_symbol=$LINUX_GNU_MAX_GLIBC_SYMBOL"

if [[ "$(printf '%s\n' "$max_glibc" "$LINUX_GNU_MAX_GLIBC_SYMBOL" | sort -V | tail -1)" != "$LINUX_GNU_MAX_GLIBC_SYMBOL" ]]; then
  echo "Binary $label requires $max_glibc, exceeding supported floor $LINUX_GNU_MAX_GLIBC_SYMBOL." >&2
  exit 1
fi

if command -v ldd >/dev/null 2>&1; then
  echo "ldd:"
  ldd "$binary" || true
fi

echo "linux-gnu linkage contract satisfied for $label."
