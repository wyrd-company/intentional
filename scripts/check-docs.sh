#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

# Generated release data is checked by release-plan and tag validation.
pages=()
for page in docs/user/*.md; do
  if [[ "$page" != "docs/user/CHANGELOG.md" ]]; then
    pages+=("$page")
  fi
done
if (( ${#pages[@]} == 0 )); then
  echo "No authored documentation pages found." >&2
  exit 1
fi

rumdl check "${pages[@]}"
ryl check docs/user/docs.yml .github/workflows/publish-docs.yml Taskfile.yml
ryl --markdown "${pages[@]}"

test "$(yq -r '.products | keys | join(",")' docs/user/docs.yml)" = "intentional"
test "$(yq -r '.products.intentional.name' docs/user/docs.yml)" = "intentional"

# Every asset a product field names is listed, and every listed asset exists.
for field in logo logo-dark favicon social-image; do
  path="$(yq -r ".products.intentional.\"$field\"" docs/user/docs.yml)"
  yq -e ".assets[] | select(. == \"$path\")" docs/user/docs.yml >/dev/null
done
for asset in $(yq -r '.assets[]' docs/user/docs.yml); do
  test -f "docs/user/$asset"
done

for page in "${pages[@]}"; do
  test "$(yq --front-matter=extract -r '.product' "$page")" = "intentional"
  test "$(yq --front-matter=extract -r 'has("docs")' "$page")" = "false"
  test -n "$(yq --front-matter=extract -r '.title' "$page")"
  yq --front-matter=extract -e '.order | type == "!!int"' "$page" >/dev/null
done

test -f docs/user/assets/demo.gif
gifsicle --info docs/user/assets/demo.gif >/dev/null
test "$(wc -c < docs/user/assets/demo.gif)" -le 3145728

test "$(rg -c '^Output docs/user/assets/demo.gif$' docs/demo.tape)" = "1"
test "$(rg -c '^Set (Shell|FontSize|Width|Height|Theme|Padding|TypingSpeed)' docs/demo.tape)" -ge "7"

if rg -n '\]\((?!https?://|assets/demo\.gif)[^)]+\)' "${pages[@]}" --pcre2; then
  echo "Unexpected local documentation link; add it to the validator." >&2
  exit 1
fi
