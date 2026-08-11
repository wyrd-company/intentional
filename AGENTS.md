---
relationships:
  references:
    - semantic-versioning
    - compatibility-and-migration
    - std-backward-compatibility
---

# Intentional

Intentional is an intent-driven polyglot release and versioning tool.

## Commands

Use `task --list` to discover repository commands. Run `task check` before
handoff.

## Repository conventions

- Treat `docs/` as the source of truth for unimplemented workflow behavior.
- Write design and specification artifacts as clean present-tense copy.
- Add or update an Intentional Change Intent for every adopter-visible change.
- Use stable recipe IDs in configuration. Recipe behavior is versioned with
  Intentional.

## Code review

Review every change against
`docs/technical-designs/semantic-versioning.yml`. Enumerate affected public
surfaces before accepting its Change Intent: command-line interface, published
crate application programming interface (API), configuration, Change Intents,
recipes, workflows, postures, durable records, and verification behavior. The
Intent records the highest Semantic Versioning (SemVer) impact among them.

An added config or recipe field is minor only when it is optional, requires
explicit selection, and omission preserves existing behavior. A required change
or changed default is major. A correction within the accepted contract is patch.

Apply `docs/policies/backward-compatibility.yml` and
`docs/standards/backward-compatibility.yml` as a separate review gate. A correct
major classification does not waive permanent record verification, migration,
schema retention, or binary retention promises.

## Owned output

Do not hand-edit generated workflow output, ownership manifests, Release Plans,
release evidence, or Cargo's lockfile. Change their authoritative source or use
the owning command.

## Documentation

User documentation is at the root of `docs/`. Designs, specifications, policy,
standards, and decisions are in their named `docs/` subdirectories.
