---
intentional: major
---

`intentional-core` breaking change: the public record and result types are marked `#[non_exhaustive]`, so code outside the crate can no longer build them with a struct literal or match them exhaustively. This covers the Release Plan types (`ReleasePlan`, `PlanSource`, `PlanReleaseUnit`, `PlanTag`, `Generator`, `ChangelogEntry`), `TagRecord` and `TagPrerequisite`, the configuration, init, intent, tag, apply, stamp, status, verify, and version result types, and the `Error`, `SchemaRecord`, and `Adapter` enums. A later added member or variant is then a minor change. New: `TagPrerequisite::new` and `ChangelogEntry::new`.
