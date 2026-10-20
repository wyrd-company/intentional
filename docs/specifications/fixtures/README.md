# Released record fixtures

Each `released-<version>/` directory holds bytes that the published
`intentional-linux-x86_64` binary of that release wrote. Tests read them to
prove that the current schemas and the current binary still accept them.
Regenerate a fixture only from the same released binary; never edit one.

All fixtures come from a fresh Git repository whose only file is a
`package.json` with `"name": "sample-library"` and `"version": "1.2.3"`.

- `config.yml`: `intentional init`, then the one npm candidate resolved as
  `independent` with Release Unit `sample-library`, then `intentional init`
  again. Versions 0.1.2, 0.1.8, and 0.2.0 write identical bytes. Version 0.1.0
  writes the earlier `packages` shape, which is before the compatibility floor.
- `released-0.2.0/release-config.yml`: `config.yml` with a `release` workspace
  tag stream added (`require-phase: after-publication`, `tag-after` the
  primary tag), committed with the tag `sample-library@1.2.3`. Version 0.2.0
  loads it.
- `released-0.2.0/release-plan.json`: `intentional plan` on the committed
  `release-config.yml`, the tag `sample-library@1.2.3`, and one committed minor
  intent for `sample-library`.
- `released-0.2.0/release-unit-tag-record.txt` and `workspace-tag-record.txt`:
  the messages of `sample-library@1.3.0` and `0.0.1`, created by
  `intentional tag --plan release-plan.json` and
  `intentional tag --plan release-plan.json --phase after-publication` after
  `intentional apply` and a commit of its output.
- `released-0.2.0/baseline-tag-record.txt`: the message of
  `sample-library@1.2.3`, created by
  `intentional tag --baseline --version workspace/release=0.1.0` on the
  committed `release-config.yml` with no tags.
- `released-0.2.0/baseline-digest-payload.json`: the canonical baseline digest
  payload whose SHA-256 digest is that baseline tag record's `plan-digest`.

# Record codec fixtures

`release-plan/` and `tag-record/` hold hand-built documents that the record
codecs decode. Each valid document is canonical: decoding then encoding it
gives its bytes. A Release Plan document ends with the one newline that
`intentional plan` prints after the encoded plan.

- `release-plan/canonical.json`: a channel plan that carries every optional
  member. Its `digest` is the SHA-256 digest of its canonical payload without
  `digest`.
- `release-plan/reordered.json`: the same plan with its keys in reverse order
  and indented. It decodes to the same plan and digest.
- `release-plan/invalid/`: `canonical.json` with one schema violation each,
  named for the violation.
