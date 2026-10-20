---
intentional: minor
---

Add `$schema` (`https://intentional.foo/schemas/0.3/release-plan`) and `source`, the commit and tree the plan was computed from, to the Release Plan that `plan` emits. Both are inside the plan digest. A plan without them keeps its recorded digest, and `tag --plan` still accepts it.
