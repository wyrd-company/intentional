# Changelog

## 0.1.1

### Fixes

- `check` fails when a `committed` projection holds a version other than the tag-derived current version, naming the release unit, file, pointer, found version, and expected version. When a release unit has an applied release that waits for its tag, every `committed` projection of the unit must hold that release version, including a prerelease above the last stable tag, so the commit of a full apply passes and a partly applied release fails. An existing changelog whose leading heading is not plain SemVer supplies no applied release to `check`, which compares projections with the tag-derived version. `tag` retains strict release validation. `status` reports drift on `injected` projections without failing `check`; neither command reads `none` projections.
