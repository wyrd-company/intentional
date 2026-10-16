#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/dist" "$temporary/homebrew-tools/Formula" "$temporary/generated"
ln -s "$root/scripts" "$temporary/scripts"
for target in linux-x86_64 linux-arm64 macos-arm64; do
  printf '%s' "$target" > "$temporary/dist/intentional-$target.tar.gz"
done

# Execute the real workflow through its publication guard, stopping before any tap write.
script="$(yq -r '.jobs.publish-homebrew-tap.steps[] |
  select(.name == "Generate, compare, and publish formula") | .run' "$root/.github/workflows/cd.yml")"
guard="${script%%install -m 0644*}"
version=1.2.10
formula="$temporary/homebrew-tools/Formula/intentional.rb"
"$root/scripts/release/render-homebrew-formula.sh" \
  "$version" "$temporary/dist" "$temporary/reference.rb"

expect_guard() {
  local label="$1" expected="$2" actual=0
  (
    cd "$temporary"
    GITHUB_REF="refs/tags/$version" RUNNER_TEMP="$temporary/generated" \
      bash -euo pipefail -c "$guard"
  ) > "$temporary/result" 2>&1 || actual=$?
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL $label: expected $expected, got $actual" >&2
    cat "$temporary/result" >&2
    exit 1
  fi
  echo "PASS $label"
}

expect_guard 'no existing formula publishes' 0
for layout in inferred explicit; do
  for existing in 1.2.9 1.2.10 1.2.11; do
    sed "s|/download/$version/|/download/$existing/|g" "$temporary/reference.rb" > "$formula"
    if [[ "$layout" == explicit ]]; then
      sed -i "2i\\  version \"$existing\"" "$formula"
    fi
    actual="$("$root/scripts/release/read-homebrew-version.sh" "$formula")"
    [[ "$actual" == "$existing" ]] || { echo "FAIL $layout version identity" >&2; exit 1; }
    expected=0
    if [[ "$existing" == 1.2.11 || ( "$existing" == "$version" && "$layout" == explicit ) ]]; then
      expected=1
    fi
    expect_guard "$layout formula at $existing" "$expected"
  done
done
cp "$temporary/reference.rb" "$formula"
printf '\n# Different formula bytes\n' >> "$formula"
expect_guard 'same version with different bytes refuses' 1
