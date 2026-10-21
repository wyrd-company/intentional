---
relationships:
  implements: intent-driven-polyglot-release
---

# Closed public types

`intentional-core` closes its public structs and enums with
`#[non_exhaustive]`, so that an added field or variant is a minor change. This
test fails when a `pub struct` or `pub enum` under `crates/core/src` has no
`#[non_exhaustive]` and is not listed in `open_types.rs`. Restricted items
(`pub(crate)`, `pub(super)`, `pub(in ...)`) and items in `#[cfg(test)]` modules
are not counted.

Run it with `cargo test -p intentional-core --test closed_public_types`.

## When the test fails

- A new public type: add `#[non_exhaustive]`. Leave it open only when an added
  member is a breaking change anyway, for example an enum whose variant set
  mirrors a published schema enum, or an input type that callers build.
- An open type on purpose: add one entry to `OPEN_TYPES` in `open_types.rs`.
  The key is the module path inside the crate (`model::Bump`,
  `adapters::format::JsonFormat`); the reason is one line.
- A stale entry: the type was removed, renamed, or closed. Remove the entry.

Opening or closing an existing public type changes the published crate API.
Classify it under `docs/technical-designs/semantic-versioning.yml`.
