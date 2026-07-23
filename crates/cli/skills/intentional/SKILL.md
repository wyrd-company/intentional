---
name: intentional
description: Operate Intentional's intent-driven polyglot versioning and release workflow safely. Use when an agent needs to initialize or adopt Intentional in a Git repository, resolve release-unit discovery, author change intents, inspect version state or drift, prepare and apply a digest-bound release plan, stamp injected build versions, create baseline or release tags, run prerelease channels, or diagnose Intentional configuration and release failures.
metadata:
  relationships:
    implements: intent-driven-polyglot-release
---

# Operate Intentional

Use the installed `intentional` binary as the authority for its supported
command surface. Run `intentional --help` and the relevant subcommand's
`--help` before relying on flags not shown here.

## Preserve the authority boundary

Treat these as Intentional-owned state:

- `.intentional/config.yml` defines release units, projections, dependency
  propagation, tags, and version interpretation.
- `.intentional/intents/*.md` declare pending user-visible changes and bumps.
- annotated Git tags record released versions and bind them to the
  interpretation contract and release-plan digest.
- manifests and changelogs are projections of that state, not competing
  version authorities.

Intentional may write its state, projected files, changelogs, and annotated
tags. It does not commit, push, publish packages, create forge releases, or open
pull requests. Perform those surrounding actions only when the user has granted
that authority and the repository's release procedure defines them.

Do not add a `v` prefix to tag versions unless an existing repository contract
explicitly requires it. Do not infer registries, publication order, privacy, or
deployment behavior from discovered manifests.

## Orient before changing state

1. Inspect the worktree, current branch, repository guidance, and release
   automation.
2. Check whether `.intentional/config.yml` exists.
3. In a configured repository, run:

   ```bash
   intentional status
   intentional check
   ```

4. Read `.intentional/config.yml` to identify exact release-unit ids,
   projection modes, tag phases, and tag prerequisites.
5. Use `-C PATH` or `--directory PATH` when the target is not the current
   directory. Shell redirections still resolve from the shell's directory, not
   from `-C`.

Treat `status` as the human-readable view of pending intents, current and next
versions, missing baselines, manifest drift, and tag-record issues. Treat
`check` as the continuous-integration validation gate.

## Choose the workflow

- No configuration: initialize the repository.
- Existing Changesets configuration: use the explicit adoption workflow.
- A change needs release notes or a version bump: author an intent.
- Pending intents are approved for release: plan, apply, commit, then tag.
- An injected projection needs a build version: stamp it.
- A prerelease line is required: use one consistent channel on plan, apply, and
  tag.
- Status reports missing baselines: establish baselines only after the
  configuration is committed.

## Initialize a repository

Run initialization from a Git worktree:

```bash
intentional init
```

Discovery recursively follows Git ignore rules and hard cache exclusions.
Package-manager workspace membership is evidence for inference,
recommendations, dependency analysis, and parity; it is not an inclusion
boundary.

The first run may write `.intentional/init-plan.yml` and exit with code `2`.
That means explicit input is required; it is not an operational failure. Read
the evidence and diagnostics, then set every candidate resolution to one of:

- `independent`: create a release unit for the candidate.
- `projection`: attach the candidate manifest to another release unit.
- `excluded`: record that the candidate is outside the managed inventory.

Edit only the plan's documented resolution fields. Do not discard extraction
diagnostics or invent cross-ecosystem identity. Rerun the same `init` command;
Intentional writes `.intentional/config.yml` only when the complete candidate
graph validates. Review and commit the resulting configuration before creating
baseline tags.

Candidates remain distinct by detector and exact path even when their native
identities match. Resolve each path explicitly; do not collapse duplicates
before the plan applies every resolution.

## Adopt a Changesets repository

When `.changeset/config.json` exists, ordinary `intentional init` preserves
Changesets authority and produces an adoption plan. Resolve its candidate,
ignored-package, contract, parity, and repository-integration requirements.
Probable release-tool proxies appear as explicit `retain` or `remove` choices
with supporting evidence, contradictory evidence, and uncertainty. Treat a
removal recommendation as best effort, never proof. Authorize `remove` only
when the mapped artifact owns the release identity and repository references
have been migrated; takeover rechecks those references before deletion.
Then use the explicit handoff:

```bash
intentional init
intentional init --take-over --dry-run
intentional init --take-over
```

Inspect the transaction's diff, commit the takeover, and only then run:

```bash
intentional tag --baseline
```

Do not delete Changesets state manually. Takeover is the rollback-capable
authority transition and limits its own write set.

## Author a change intent

Use release-unit ids from `.intentional/config.yml`, not package names guessed
from directory names. Pass every value explicitly so an agent never depends on
interactive prompts:

```bash
intentional add \
  --release-unit sample-library:minor \
  --release-unit sample-application:patch \
  --message "Add a user-visible capability."
```

Use `major`, `minor`, or `patch` according to the repository's configured
pre-1.0 mapping and compatibility contract. Write concise changelog prose about
the user-visible outcome. One intent may bump multiple release units when they
represent one coherent change. Review the generated Markdown and commit it with
the implementation; do not edit versions directly in projected manifests.

Use `--dry-run` to inspect the planned intent file before writing it.

## Prepare and apply a release

Preserve the exact plan before applying because `apply` consumes included
intents:

```bash
intentional status
intentional check
intentional plan > release-plan.json
intentional apply --dry-run
intentional apply
```

The plan is canonical, digest-bound JSON. Do not reformat or hand-edit it.
Review the plan, operation preview, and resulting diff. Verify that projected
versions, internal dependency ranges, changelogs, and consumed intents match
the accepted release. The surrounding harness must then commit the applied
tree.

After that commit, create annotated release records against the same saved
plan:

```bash
intentional tag --plan release-plan.json
```

If configuration requires an executor phase, pass the phase that reflects what
the surrounding release process has actually completed:

```bash
intentional tag --plan release-plan.json --phase before-publication
```

Never claim a publication phase speculatively. Intentional verifies phase and
`tag-after` prerequisites and computes deterministic tag order. Tags remain
local until the authorized harness pushes them.

## Establish baseline tags

Use baselines when `status` or `check` reports that a configured release unit
has no tag-derived authority. Commit configuration and verify existing manifest
versions first, then preview and create baselines:

```bash
intentional tag --baseline --dry-run
intentional tag --baseline
```

If projections cannot supply an unambiguous version, pass explicit values with
repeatable `--version release-unit=X.Y.Z` or `--version workspace/tag=X.Y.Z`.
Do not combine `--baseline` with `--channel` or `--plan`.

## Stamp injected build versions

Use `stamp` only for projections configured as `injected`. It writes computed
build versions without consuming intents or updating changelogs:

```bash
intentional stamp --dry-run
intentional stamp
intentional stamp --prerelease alpha
```

The prerelease form composes the next version with first-parent tag height.
Treat stamped output as build material unless repository guidance says it is
committed.

## Run a prerelease channel

Use the same channel name throughout one release attempt and preserve its plan:

```bash
intentional plan --channel beta > release-plan.json
intentional apply --channel beta --dry-run
intentional apply --channel beta
intentional tag --channel beta --plan release-plan.json
```

A channel apply retains intents. A later channel-less apply consolidates the
prerelease changelog into the final release. Do not manually consume the intents
between those operations.

## Diagnose failures

- Run the failing command with the same `-C`, channel, plan, and phase inputs;
  changing inputs can hide state divergence.
- Rerun `status` and `check`, then inspect the named config, intent, manifest,
  changelog, plan, or tag record.
- Treat manifest drift as evidence that a projection no longer matches tag
  authority; do not silence it by changing tags or manifests without proving
  which state is correct.
- Treat a plan-digest mismatch as stale or modified release evidence. Generate
  and approve a new plan rather than editing the old plan or bypassing
  verification.
- Treat exit code `2` from `init` as unresolved input or repository edits. Treat
  exit code `1` as invalid input or operational failure.
- If a command would require committing, pushing, publishing, or forge access,
  stop at the Intentional boundary unless that external action is explicitly in
  scope.

## Complete the handoff

Report the exact commands run, release units and computed versions, plan path
and digest, files changed, tags created, validation results, and every external
action still owned by the surrounding harness. Never report a dry run as an
applied mutation or a locally created tag as pushed or published.
