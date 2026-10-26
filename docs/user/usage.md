---
title: Usage
order: 4
product: intentional
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

## Initialize before declaring changes

For a repository without `.intentional/config.yml`, follow the
[initialization guide](https://wyrd.foo/docs/intentional/initialization). It covers
candidate resolutions, already released repositories, Go modules, baseline
versions, and Changesets adoption. Once configuration and version authority are
established, use the release workflow below.

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
versions, suspended release blockers, manifest drift, tag-record issues, and
missing baselines. A drift row names the projection file by its path from the
repository root.
`check` validates configuration, intents, tag records, baselines, and
deterministic planning for continuous integration.

Tags created before Intentional carry no Intentional record. Both lightweight
tags and annotated tags without any Intentional record field are tolerated as
pre-Intentional history: they supply the current version and raise no issue.
`tag --baseline` leaves such a tag in place instead of retagging it. An
annotated tag that carries any Intentional record field is validated in full,
so a partial or corrupt record fails closed.

## Preview the plan

`plan` writes canonical, digest-bound release-plan JSON to standard output
without changing anything. It includes changed release units, contributing
intents, release notes, expected tags, required phases, and tag order:

```bash
intentional plan > release-plan.json
```

The sealed payload contains the interpretation contract, generator identity,
old and new versions, contributing intent ids, rendered release notes,
release-unit and workspace tags, phase declarations, `tag-after` prerequisites,
and tag order. It contains no publication graph. Preserve the exact output;
do not hand-edit or reformat the sealed plan.

## Project the versions

`apply` writes release versions into committed projections, rewrites internal
dependency ranges, updates each release unit's changelog, and consumes the
included intents. A file that several release units project is written once,
with the field of each released unit. It edits only the working tree:

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

Without `--plan`, a final release is recovered from the intents deleted by the
release commit; a channel release is recovered from its retained intents.

If a tag requires an executor phase, declare it explicitly:

```bash
intentional tag --plan release-plan.json --phase before-publication
```

The phase declaration does not assert or verify publication. External
continuous delivery owns publication sequencing and registry evidence.
Intentional verifies `tag-after` prerequisites before creating a dependent
tag. An exact existing selected record satisfies the operation, so repeating
the same dry run or tag command verifies completed records and creates only
missing records. Any conflicting target, version, contract, plan digest,
baseline, or prerequisite fails closed. Intentional never creates the
surrounding commit or pushes tags.

For repository self-releases, verify tag acceptance with the materialized
workspace binary (`task self-release:verify`) and create annotated tags only
through the guarded operator path (`CREATE_ACK=create-annotated-tag task
self-release:tag`). Do not use an older installed Intentional for tag creation
after apply.

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
intentional tag --channel beta --plan release-plan.json
```

The first release is `X.Y.Z-beta.1`; later iterations advance from existing
tag records. A channel `apply` retains intents; a later channel-less `apply`
consolidates the prerelease changelog sections into the final release.

## Self-hosted repository releases

When the sealed plan was generated by the immediately preceding published
Intentional but tagging requires the newly materialized workspace binary (for
example deterministic tagger headers introduced in the release being applied),
never call the installed older binary for tag creation.

Verification stays dry-run safe by default:

```console
SEALED_PLAN=/path/to/release-plan.json task self-release:verify
```

Real annotated tag creation uses the same build, version-agreement, and sealed-plan
checks, but runs the materialized `target/release/intentional` without
`--dry-run` only through the guarded operator path:

```console
SEALED_PLAN=/path/to/release-plan.json \
CREATE_ACK=create-annotated-tag \
task self-release:tag
```

`CREATE_ACK` must be exactly `create-annotated-tag`. CI and routine checks use
only `self-release:verify` and `scripts/release/test-self-hosted-tag.sh`; they
never invoke the create path.

## Dry runs

Every mutating command supports `--dry-run` and prints the same operation set as
the real invocation:

```console
intentional init --dry-run
intentional init --take-over --dry-run
intentional add --release-unit library:patch --message "Correct a defect." --dry-run
intentional apply --dry-run
intentional stamp --prerelease alpha --dry-run
intentional tag --dry-run
intentional tag --baseline --dry-run
```

## GitHub Action

The plumbing action downloads a released Linux binary and runs one command.
Pin it to a plain SemVer tag. The action exposes canonical `plan` JSON and a
changed-release-unit boolean. It accepts `status`, `plan`, `stamp`, `apply`, and
`tag`; `channel` applies to release commands, `prerelease` applies to `stamp`,
and `dry-run` applies to mutating commands.
