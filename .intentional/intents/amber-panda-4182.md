---
intentional: major
---

Add `$schema` (`https://intentional.foo/schemas/0.3/release-plan`) and `source`, the commit and tree the plan was computed from, to the Release Plan that `plan` emits. Both are inside the plan digest. A plan without them keeps its recorded digest, and `tag --plan` still accepts it.

`intentional-core` breaking change: `ReleasePlan` gains the public fields `schema` and `source`, so code that builds a `ReleasePlan` with a struct literal or matches it exhaustively must name them.
