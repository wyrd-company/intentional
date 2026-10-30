---
intentional: major
---

Configuration contract `contract-2` replaces `contract-1`. A bump name always states the significance of a change: before 1.0.0, `major` gives `0.(x+1).0`, and `minor` and `patch` give `0.x.(y+1)`. `settings.pre-1-0-bump-mapping` is removed. Every command except `init` refuses a `contract-1` configuration and says to run `intentional init`, which upgrades it, keeps its comments, and accepts release tags created under `contract-1`. A `component` configuration with pending intents gets a reviewable `bump-translation` choice in the init plan, or `init --bump-translation parity|significance`.

An intent can graduate a `0.x` release unit to exactly 1.0.0 with `{ bump: major, graduate: true }`, or `add --graduate <id>`. Graduation is the only way to reach 1.0.0. It graduates a whole fixed group, or the releasing members of a linked group, and `check` fails when the release unit is already at 1.0.0 or later. `status`, the release plan (`graduates`), and the changelog report the graduation.

Changesets adoption translates pending changesets once. The default `bump-translation: parity` keeps the versions that Changesets would release, so `major` at `0.x` becomes `major` with `graduate`, and `minor` becomes `major`; each renamed bump gets a note in the intent. `significance` keeps the bump names, and the parity comparison reports the version changes as `expected-difference`.
