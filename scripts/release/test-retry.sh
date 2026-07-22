#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/bin"

cat > "$temporary/bin/curl" <<'MOCK_CURL'
#!/usr/bin/env bash
set -euo pipefail
output=""
while (($#)); do
  if [[ "$1" == "--output" ]]; then
    output="$2"
    shift 2
  else
    shift
  fi
done
cp "$MOCK_RESPONSE" "$output"
printf '%s' "$MOCK_STATUS"
MOCK_CURL

cat > "$temporary/bin/cargo" <<'MOCK_CARGO'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$MOCK_CARGO_LOG"
if [[ "$1" == "package" ]]; then
  crate=""
  while (($#)); do
    if [[ "$1" == "-p" ]]; then
      crate="$2"
      break
    fi
    shift
  done
  mkdir -p target/package
  printf '%s' "$MOCK_CRATE_BYTES" > "target/package/${crate}-${MOCK_VERSION}.crate"
fi
MOCK_CARGO

cat > "$temporary/bin/gh" <<'MOCK_GH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$MOCK_GH_LOG"
if [[ "$1 $2" == "release view" ]]; then
  if [[ " $* " == *" --json isDraft,isPrerelease "* ]]; then
    printf 'false\tfalse\n'
    exit 0
  fi
  if [[ " $* " == *" --json assets "* ]]; then
    printf '1\n'
    exit 0
  fi
  [[ "$MOCK_RELEASE_EXISTS" == "1" ]]
  exit
fi
if [[ "$1 $2" == "release download" ]]; then
  pattern=""
  destination=""
  while (($#)); do
    case "$1" in
      --pattern) pattern="$2"; shift 2 ;;
      --dir) destination="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  cp "$MOCK_REMOTE_ASSETS/$pattern" "$destination/$pattern"
fi
MOCK_GH

chmod +x "$temporary/bin"/*

version="$(jq -r '.version' "$root/npm/package.json")"
pack_directory="$temporary/pack"
mkdir -p "$pack_directory"
npm pack "$root/npm" \
  --pack-destination "$pack_directory" \
  --ignore-scripts \
  --json \
  --userconfig=/dev/null \
  >/dev/null
npm_archive="$(find "$pack_directory" -type f -name '*.tgz' -print -quit)"
npm_integrity="sha512-$(openssl dgst -sha512 -binary "$npm_archive" | base64 -w0)"
npm_response="$temporary/npm.json"
jq -n \
  --arg version "$version" \
  --arg integrity "$npm_integrity" \
  '{versions: {($version): {dist: {integrity: $integrity}}}}' \
  > "$npm_response"

PATH="$temporary/bin:$PATH" \
  MOCK_RESPONSE="$npm_response" \
  MOCK_STATUS=200 \
  "$root/scripts/release/ensure-npm.sh" "$root/npm" "$version" \
  >/dev/null

jq --arg version "$version" \
  '.versions[$version].dist.integrity = "sha512-different"' \
  "$npm_response" > "$temporary/npm-mismatch.json"
if PATH="$temporary/bin:$PATH" \
  MOCK_RESPONSE="$temporary/npm-mismatch.json" \
  MOCK_STATUS=200 \
  "$root/scripts/release/ensure-npm.sh" "$root/npm" "$version" \
  >/dev/null 2>&1; then
  echo "npm retry check accepted different published bytes." >&2
  exit 1
fi

crate="sample-crate"
crate_bytes="sample crate bytes"
crate_checksum="$(printf '%s' "$crate_bytes" | sha256sum | cut -d' ' -f1)"
crate_response="$temporary/crate.json"
jq -cn \
  --arg version "$version" \
  --arg checksum "$crate_checksum" \
  '{vers: $version, cksum: $checksum}' \
  > "$crate_response"
cargo_log="$temporary/cargo.log"

cd "$temporary"
PATH="$temporary/bin:$PATH" \
  MOCK_CARGO_LOG="$cargo_log" \
  MOCK_CRATE_BYTES="$crate_bytes" \
  MOCK_RESPONSE="$crate_response" \
  MOCK_STATUS=200 \
  MOCK_VERSION="$version" \
  "$root/scripts/release/ensure-crate.sh" "$crate" "$version" \
  >/dev/null
if rg '^publish .*--locked$' "$cargo_log"; then
  echo "crate retry check attempted a second publication." >&2
  exit 1
fi

artifacts="$temporary/artifacts"
mkdir -p "$artifacts"
printf linux-x64 > "$artifacts/intentional-linux-x86_64.tar.gz"
printf linux-arm64 > "$artifacts/intentional-linux-arm64.tar.gz"
printf macos-arm64 > "$artifacts/intentional-macos-arm64.tar.gz"
printf windows-x64 > "$artifacts/intentional-windows-x86_64.zip"
(
  cd "$artifacts"
  sha256sum intentional-* > SHA256SUMS
)
formula_one="$temporary/intentional-one.rb"
formula_two="$temporary/intentional-two.rb"
"$root/scripts/release/render-homebrew-formula.sh" "$version" "$artifacts" "$formula_one"
"$root/scripts/release/render-homebrew-formula.sh" "$version" "$artifacts" "$formula_two"
cmp "$formula_one" "$formula_two"
rg -q "version \"$version\"" "$formula_one"

release_notes="$temporary/release-notes.md"
"$root/scripts/release/extract-release-notes.sh" \
  "$version" "$root/CHANGELOG.md" "$release_notes"
test "$(head -n 1 "$release_notes")" = "## $version"
if "$root/scripts/release/extract-release-notes.sh" \
  "9.9.9" "$root/CHANGELOG.md" "$temporary/missing-notes.md" \
  >/dev/null 2>&1; then
  echo "release-note check accepted a missing version section." >&2
  exit 1
fi

release_repository="$temporary/release-repository"
remote_assets="$temporary/remote-assets"
mkdir -p "$release_repository" "$remote_assets"
cp "$artifacts"/* "$remote_assets/"
printf 'Release notes\n' > "$release_repository/notes.md"
(
  cd "$release_repository"
  git init -q
  git config user.name "Release Test"
  git config user.email "release-test@example.invalid"
  git add notes.md
  git commit -q -m "Create release fixture"
  git tag -a "$version" -m "$version"
  gh_log="$temporary/gh.log"
  PATH="$temporary/bin:$PATH" \
    MOCK_GH_LOG="$gh_log" \
    MOCK_RELEASE_EXISTS=1 \
    MOCK_REMOTE_ASSETS="$remote_assets" \
    "$root/scripts/release/ensure-github-release.sh" \
      "$version" "$artifacts" notes.md \
      >/dev/null

  printf different > "$remote_assets/intentional-linux-x86_64.tar.gz"
  if PATH="$temporary/bin:$PATH" \
    MOCK_GH_LOG="$gh_log" \
    MOCK_RELEASE_EXISTS=1 \
    MOCK_REMOTE_ASSETS="$remote_assets" \
    "$root/scripts/release/ensure-github-release.sh" \
      "$version" "$artifacts" notes.md \
      >/dev/null 2>&1; then
    echo "GitHub Release retry check accepted different asset bytes." >&2
    exit 1
  fi
)

echo "release retry checks passed"
