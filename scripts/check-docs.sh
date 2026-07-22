#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

rumdl check docs/*.md
ryl check docs/docs.yml .github/workflows/publish-docs.yml Taskfile.yml
ryl --markdown docs/*.md

test "$(yq -r '.name' docs/docs.yml)" = "intentional"
test "$(yq -r '.assets | length' docs/docs.yml)" = "1"
test "$(yq -r '.assets[0]' docs/docs.yml)" = "assets/demo.gif"

for page in docs/*.md; do
  test "$(yq --front-matter=extract -r '.docs' "$page")" = "true"
  test -n "$(yq --front-matter=extract -r '.title' "$page")"
  yq --front-matter=extract -e '.order | type == "!!int"' "$page" >/dev/null
done

test -f docs/assets/demo.gif
gifsicle --info docs/assets/demo.gif >/dev/null
test "$(wc -c < docs/assets/demo.gif)" -le 3145728

test "$(rg -c '^Output docs/assets/demo.gif$' docs/demo.tape)" = "1"
test "$(rg -c '^Set (Shell|FontSize|Width|Height|Theme|Padding|TypingSpeed)' docs/demo.tape)" -ge "7"

if rg -n '\]\((?!https?://|assets/demo\.gif)[^)]+\)' docs/*.md --pcre2; then
  echo "Unexpected local documentation link; add it to the validator." >&2
  exit 1
fi
