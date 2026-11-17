---
intentional: major
---

Moving to this release. Configuration stays at `contract-2`, and the tag records and Release Plans this release writes carry `contract: contract-2`; records made under `contract-1` stay valid history. A repository still on a `contract-1` configuration (Intentional 0.3.x or earlier) runs `intentional init`, reviews any bump translation the init plan proposes, and commits the upgraded `.intentional/config.yml` with any rewritten intents; every other command refuses `contract-1`.

A repository on 0.4.x runs `intentional model` and commits `.intentional/release-model.json` with root `.gitattributes`, so that a release process template can read the Release Model; from then on `check` fails and `status` reports `release-model-stale` when the model falls behind the configuration. Tools that read ordinary `init --json` output find the configuration under `inferred-config`. Tags that earlier releases created, including releases made under `contract-1` with `pre-1-0-bump-mapping: component`, verify with `intentional verify <tag>`, which rebuilds them under the contract they were made under.

Intentional generates no CI workflow and runs none. A release process template, rendered by a template tool such as Toha, reads the Release Model and owns the workflow, its commits, pushes, publication and credentials.
