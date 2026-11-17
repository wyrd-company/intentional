---
intentional: major
---

`intentional-core` breaking change: `ReleasePlan::to_canonical_json` is replaced by `ReleasePlan::encode`, which refuses a plan that breaks the published schema, and `Error` gains the `Schema` variant, so exhaustive matches on `Error` must handle it. New: `ReleasePlan::decode`, the `TagRecord` codec, `baseline_digest`, and the `schema` module.
