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
