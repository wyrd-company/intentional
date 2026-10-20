---
intentional: minor
---

Add `intentional verify <tag>`, which verifies one established Release from its annotated tag with Git history only: the tag record and tagger, the plan recomputed at the commit the release was planned from against the recorded digest, the files the release commit materialized, and the tag records of every identity tag. It exits 1 with a typed diagnostic on any failure and prints a structured report with `--json`. A digest difference for a tag produced by another Intentional version is an advisory. `intentional-core` adds `verify_release` and its report types.
