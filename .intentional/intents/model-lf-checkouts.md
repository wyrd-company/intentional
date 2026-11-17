---
intentional: minor
---

Keep Release Models fresh across CRLF Git checkouts: `model` and initialization
ensure `.intentional/release-model.json text eol=lf` in root `.gitattributes`,
preserving unrelated rules. Commit the attributes file with the model. `model`
reports `written` when either file needs repair; dry-run changes neither file.

`intentional-core` adds the optional `release_model_stale_message` formatter,
shared by check and human status so that both report the same repair text.
