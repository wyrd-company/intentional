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
label="${2:?target label is required}"

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

case "$label" in
  *x86_64*)
    target_arch=x86_64
    expected_interpreter=/lib64/ld-linux-x86-64.so.2
    expected_needed=(
      libgcc_s.so.1
      librt.so.1
      libpthread.so.0
      libm.so.6
      libdl.so.2
      libc.so.6
    )
    ;;
  *aarch64*|*arm64*)
    target_arch=aarch64
    expected_interpreter=/lib/ld-linux-aarch64.so.1
    expected_needed=(
      libgcc_s.so.1
      libpthread.so.0
      libm.so.6
      libdl.so.2
      libc.so.6
    )
    ;;
  *)
    echo "Unsupported Linux GNU target label: $label" >&2
    exit 1
    ;;
esac

echo "label=$label"
echo "target_arch=$target_arch"

file_output="$(file "$binary")"
echo "$file_output"

if ! grep -Fq 'ELF' <<<"$file_output"; then
  echo "Binary is not an ELF executable: $binary" >&2
  exit 1
fi

if ! grep -Fq 'dynamically linked' <<<"$file_output"; then
  echo "Binary must be dynamically linked: $binary" >&2
  exit 1
fi

if ! grep -Fq 'pie executable' <<<"$file_output"; then
  echo "Binary must be a position-independent executable: $binary" >&2
  exit 1
fi

readelf -l "$binary" | sed -n '/INTERP/,+1p'
interpreter="$(readelf -l "$binary" | sed -n 's/.*interpreter: \(.*\)]/\1/p')"
echo "interpreter=$interpreter"
echo "expected_interpreter=$expected_interpreter"

if [[ "$interpreter" != "$expected_interpreter" ]]; then
  echo "Unexpected interpreter for $label: $interpreter" >&2
  exit 1
fi

needed_libs="$(
  readelf -d "$binary" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' | sort
)"
expected_needed_sorted="$(
  printf '%s\n' "${expected_needed[@]}" | sort
)"
echo "needed_libraries:"
printf '%s\n' "$needed_libs"
echo "expected_needed_libraries:"
printf '%s\n' "$expected_needed_sorted"

if [[ "$needed_libs" != "$expected_needed_sorted" ]]; then
  echo "NEEDED library set mismatch for $label." >&2
  echo "unexpected libraries (present in binary, not expected):" >&2
  comm -23 <(printf '%s\n' "$needed_libs") <(printf '%s\n' "$expected_needed_sorted") >&2 || true
  echo "missing libraries (expected, not present in binary):" >&2
  comm -13 <(printf '%s\n' "$needed_libs") <(printf '%s\n' "$expected_needed_sorted") >&2 || true
  exit 1
fi

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
  echo "Binary $label requires $max_glibc, exceeding supported ceiling $LINUX_GNU_MAX_GLIBC_SYMBOL." >&2
  exit 1
fi

host_arch="$(uname -m)"
run_ldd=false
case "$host_arch" in
  x86_64) [[ "$target_arch" == x86_64 ]] && run_ldd=true ;;
  aarch64|arm64) [[ "$target_arch" == aarch64 ]] && run_ldd=true ;;
esac

if [[ "$run_ldd" == true ]] && command -v ldd >/dev/null 2>&1; then
  echo "ldd:"
  ldd "$binary"
else
  echo "ldd: skipped (readelf-only inspection for $label on $host_arch host)"
fi

echo "linux-gnu linkage contract satisfied for $label."
