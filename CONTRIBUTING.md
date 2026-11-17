---
relationships:
  references:
    - semantic-versioning
    - pol-backward-compatibility
    - std-backward-compatibility
---

# Contributing to Intentional

Start with an issue or discussion for changes that alter Intentional's public
release model. Keep implementation, schemas, examples, and user documentation
in agreement. Run `task check` before requesting review.

Every adopter-visible change needs a Change Intent under
`.intentional/intents/`. Its bump states the highest Semantic Versioning
(SemVer) impact across the complete change, not only the code file being edited.

## Code review

Reviewers use `docs/technical-designs/semantic-versioning.yml` to evaluate the
command-line interface, published crate application programming interface
(API), configuration, Change Intent format, recipes, generated workflows,
security postures, durable records, and verification behavior.

Classify required or incompatible config and recipe changes as major. Classify
an optional capability as minor only when existing users must select it and
omission preserves their behavior. Classify a correction within the existing
contract as patch. Correct an understated Change Intent before acceptance.

Backward compatibility is a separate acceptance gate. Review against
`docs/policies/backward-compatibility.yml` and
`docs/standards/backward-compatibility.yml`. A SemVer major identifies impact;
it does not permit breaking Intentional's permanent compatibility promises.

## Public types in `intentional-core`

Close each new public struct or enum with `#[non_exhaustive]`, so that an added
member is a minor change. A test enforces this and holds the list of types that
are open on purpose; see `crates/core/tests/closed_public_types/README.md`.
