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
  if ! grep -Fq "$asset" "$root/scripts/action/install-intentional.sh"; then
    echo "install-intentional.sh is missing asset $asset" >&2
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

assert_archive_layout() {
  local artifact="$1"
  local format="$2"
  local workspace binary archive extracted executable_name

  workspace="$(mktemp -d)"

  if [[ "$format" == zip ]]; then
    binary="$workspace/stub-intentional.exe"
    executable_name=intentional.exe
  else
    binary="$workspace/stub-intentional"
    executable_name=intentional
  fi

  cat >"$binary" <<'EOF'
#!/usr/bin/env bash
echo intentional-archive-layout-stub
EOF
  chmod 0755 "$binary"

  archive="$workspace/${artifact}.${format}"
  python3 "$root/scripts/release/package-archive.py" \
    --artifact "$artifact" \
    --binary "$binary" \
    --format "$format" \
    --output "$archive"

  extracted="$workspace/extracted"
  mkdir -p "$extracted"

  case "$format" in
    tar.gz)
      tar -xzf "$archive" -C "$extracted"
      ;;
    zip)
      unzip -q "$archive" -d "$extracted"
      ;;
    *)
      rm -rf "$workspace"
      echo "Unsupported archive format for layout check: $format" >&2
      exit 1
      ;;
  esac

  for entry in "$executable_name" LICENSE README.md; do
    if [[ ! -f "$extracted/$artifact/$entry" ]]; then
      rm -rf "$workspace"
      echo "Archive $archive is missing top-level file $artifact/$entry" >&2
      exit 1
    fi
  done

  if [[ ! -x "$extracted/$artifact/$executable_name" ]]; then
    rm -rf "$workspace"
    echo "Archive $archive must ship an executable $artifact/$executable_name" >&2
    exit 1
  fi

  rm -rf "$workspace"
  echo "archive layout verified for $artifact ($format)"
}

assert_archive_layout intentional-linux-x86_64 tar.gz
assert_archive_layout intentional-linux-arm64 tar.gz
assert_archive_layout intentional-macos-arm64 tar.gz
assert_archive_layout intentional-windows-x86_64 zip

echo "Release consumer contract asset names and archive layout are unchanged."
