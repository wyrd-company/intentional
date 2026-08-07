#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

set -euo pipefail

rumdl check docs/*.md
ryl check docs/docs.yml .github/workflows/publish-docs.yml Taskfile.yml
# ryl lints the contents of a block scalar as YAML rather than as opaque text,
# so a fenced ```yaml example inside a `|-` section reports one indentation
# error per nested line. No document linted below can carry a fenced YAML
# example. State configuration shapes in prose here, and put runnable YAML in
# docs/*.md, which `rumdl check docs/*.md` and `ryl --markdown docs/*.md` lint
# as Markdown.
ryl check \
  docs/features/*.yml \
  docs/specifications/*.yml \
  docs/technical-designs/*.yml \
  schemas/*.yml
ryl --markdown docs/*.md

cmp docs/specifications/config.json-schema.yml schemas/config.yml
cmp docs/specifications/executor-init-plan.json-schema.yml schemas/executor-init-plan.yml
cmp docs/specifications/init-plan.json-schema.yml schemas/init-plan.yml
cmp docs/specifications/release-plan.json-schema.yml schemas/release-plan.yml
cmp docs/specifications/tag-record.json-schema.yml schemas/tag-record.yml
cmp docs/specifications/workflow-diff.json-schema.yml schemas/workflow-diff.yml

test "$(yq -r '.name' docs/docs.yml)" = "intentional"
test "$(yq -r '.assets | length' docs/docs.yml)" = "3"
test "$(yq -r '.assets[0]' docs/docs.yml)" = "assets/demo.gif"
test "$(yq -r '.assets[1]' docs/docs.yml)" = "assets/publish-workflow.mmd"
test "$(yq -r '.assets[2]' docs/docs.yml)" = "assets/publish-workflow.svg"
test -f docs/publish-workflow.md
test "$(rg -c '\]\(assets/publish-workflow\.svg\)' docs/publish-workflow.md)" = "1"

for page in docs/*.md; do
  test "$(yq --front-matter=extract -r '.docs' "$page")" = "true"
  test -n "$(yq --front-matter=extract -r '.title' "$page")"
  yq --front-matter=extract -e '.order | type == "!!int"' "$page" >/dev/null
done

test -f docs/assets/demo.gif
gifsicle --info docs/assets/demo.gif >/dev/null
test "$(wc -c < docs/assets/demo.gif)" -le 3145728
test -f docs/assets/publish-workflow.svg
test -f docs/assets/publish-workflow.mmd
diagram_dir="$(mktemp -d)"
trap 'rm -rf "$diagram_dir"' EXIT
npx -y @mermaid-js/mermaid-cli@11.16.0 \
  -i docs/assets/publish-workflow.mmd \
  -o "$diagram_dir/publish-workflow.svg" \
  -b transparent
cmp docs/assets/publish-workflow.svg "$diagram_dir/publish-workflow.svg"
if rg -q '<foreignObject' docs/assets/publish-workflow.svg; then
  echo "Published diagrams must use portable SVG text." >&2
  exit 1
fi
test "$(rg -o '<text' docs/assets/publish-workflow.svg | wc -l)" -ge 20

test "$(rg -c '^Output docs/assets/demo.gif$' docs/demo.tape)" = "1"
test "$(rg -c '^Set (Shell|FontSize|Width|Height|Theme|Padding|TypingSpeed)' docs/demo.tape)" -ge "7"

if rg -n '\]\((?!https?://|assets/(?:demo\.gif|publish-workflow\.svg)|publish-workflow\.md|usage\.md#publish-to-a-registry-for-the-first-time)[^)]+\)' docs/*.md --pcre2; then
  echo "Unexpected local documentation link; add it to the validator." >&2
  exit 1
fi
