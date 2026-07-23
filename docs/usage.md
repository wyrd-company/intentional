---
docs: true
title: Usage
order: 3
relationships:
  implements: intent-driven-polyglot-release
---

A release has three Intentional-owned moves: declare change intent, project the
accepted plan into the working tree, and create annotated release records. The
surrounding harness owns review, commits, pushes, publication, and forge
operations.

Every command accepts `-C` / `--directory` to point at a workspace other than
the current directory, and every mutating command accepts `--dry-run` to print
its operations without touching the filesystem or Git.

## Load the agent workflow

`skill` prints the complete agent-facing workflow guide embedded in the
installed Intentional version:

```bash
intentional skill
```

The output is a valid `SKILL.md` and can be installed directly into an agent's
skill directory. Keeping the guide in the binary makes its command and safety
guidance travel with the CLI version it describes.

## Bootstrap the workspace

`init` requires a Git repository. It recursively discovers supported manifests
within the repository's Git-ignore and hard-cache boundary. Package-manager
workspace membership informs recommendations, dependency analysis, and
Changesets parity but never limits discovery.

```bash
intentional init
```

Every discovery result first appears as a candidate in
`.intentional/init-plan.yml`. Resolve each candidate as:

- `independent`: create a release unit;
- `projection`: attach the manifest to another release unit; or
- `excluded`: record why the manifest is outside Intentional's inventory.

Rerun `intentional init` after setting the explicit resolutions. Intentional
writes `.intentional/config.yml` only after the complete candidate graph
validates. This prevents discovery guesses from silently becoming release
authority.

Candidates retain detector/path identity even when multiple manifests declare
the same native name. Explicit resolutions determine ownership before final
configuration validation.

During Changesets adoption, a manifest mapped onto another artifact can expose
an explicit `retain` or `remove` integration choice. Probable-proxy diagnostics
separate supporting and contradictory structural evidence and state their
uncertainty. Prose and semantic keyword matching are not evidence. A removal is
performed only during explicit takeover, after the resolution authorizes it and
a fresh repository-use check finds no non-Changesets references.

## Declare an intent

`add` records the intended bump for one or more release units. Repeat
`--release-unit id:major|minor|patch` and provide changelog prose:

```bash
intentional add \
  --release-unit sample-library:minor \
  --release-unit sample-application:patch \
  --message "Add a user-visible capability."
```

Run with no flags to be prompted for the release unit, bump, and message instead.
Either way, `add` writes a memorable-slug Markdown file under
`.intentional/intents/`, for example:

```markdown
---
sample-library: minor
sample-application: patch
---

Add a user-visible capability.
```

## Inspect pending state

```bash
intentional status
intentional check
```

`status` lists pending intents, tag-derived current versions, projected next
versions, manifest drift, tag-record issues, and missing baselines.
`check` validates configuration, intents, tag records, baselines, and
deterministic planning for continuous integration.

## Preview the plan

`plan` writes canonical, digest-bound release-plan JSON to standard output
without changing anything. It includes changed release units, contributing
intents, release notes, expected tags, required phases, and tag order:

```bash
intentional plan > release-plan.json
```

## Project the versions

`apply` writes release versions into committed projections, rewrites internal
dependency ranges, updates each release unit's changelog, and consumes the
included intents. It edits only the working tree:

```bash
intentional apply
```

The surrounding harness owns the commit:

```bash
git add -A
git commit -m "chore: apply release"
```

## Tag the release

After the harness commits the applied state, `tag` creates the annotated
primary, projection, and workspace records selected by the plan. Pass the saved
plan to verify its digest and expected target state:

```bash
intentional tag --plan release-plan.json
```

If a tag requires an executor phase, declare it explicitly:

```bash
intentional tag --plan release-plan.json --phase before-publication
```

Intentional verifies `tag-after` prerequisites before creating a dependent
tag. It never creates the surrounding commit or pushes tags.

## Stamp build versions

For `injected` projections, `stamp` writes the computed version without touching
changelogs or intents. Add `--prerelease` to compose the next version with the
first-parent commit height since the latest matching tag (for example
`1.3.0-alpha.5`):

```bash
intentional stamp
intentional stamp --prerelease alpha
```

## Channel releases

`plan`, `apply`, and `tag` accept `--channel` to cut a prerelease line whose
state is derived from existing tags:

```bash
intentional plan --channel beta > release-plan.json
intentional apply --channel beta
git add -A
git commit -m "chore: apply beta release"
intentional tag --channel beta
```

A channel `apply` retains intents; a later channel-less `apply` consolidates the
prerelease changelog sections into the final release.

## Adopt a Changesets repository

When `.changeset/config.json` exists, ordinary `init` preserves Changesets as
the authority and writes an adoption plan. Resolve candidate ownership,
ignored-package disposition, contract differences, and the reported repository
integrations until the plan proves release parity. Then preview and perform the
explicit takeover:

```console
intentional init
intentional init --take-over --dry-run
intentional init --take-over
git add -A
git commit -m "Adopt Intentional"
intentional tag --baseline
```

Takeover changes only Intentional and recognized Changesets state in one
rollback-capable transaction. Repository-specific scripts and workflows remain
the user's responsibility. Baseline tags are created against the externally
committed takeover state, so the authority boundary stays explicit.
