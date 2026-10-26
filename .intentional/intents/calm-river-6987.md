---
intentional: major
intentional-docs: patch
---

`check` fails when a `committed` projection holds a version other than the tag-derived current version, naming the release unit, file, pointer, found version, and expected version. The commit of an applied release that waits for its tag still passes. `injected` and `none` projections stay exempt; `status` reports their drift.
