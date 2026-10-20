---
name: intentional
description: Operate Intentional's intent-driven polyglot versioning and release workflow safely. Use when an agent needs to initialize or adopt Intentional in a Git repository, resolve release-unit discovery, author change intents, inspect version state or drift, prepare and apply a digest-bound release plan, stamp injected build versions, create baseline or release tags, write the committed Release Model for process templates, run prerelease channels, or diagnose Intentional configuration and release failures.
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

A Release Unit's `release-record` (`required` by default, or `none`) states
whether the surrounding release process creates a release record, such as a
GitHub Release, for each release. Intentional creates none under either value,
and its commands behave the same for both. Do not create a release record for a
unit set to `none`.

Do not add a `v` prefix to primary or workspace tag versions. Use one on a
projection tag only when an ecosystem contract requires it. Do not infer
registries, publication order, privacy, or deployment behavior from discovered
manifests.

## Keep the Release Model current

Run `intentional model` after changing Release Workspace configuration or manifest
identities. Commit `.intentional/release-model.json` with those changes. Templates
read this file, which contains inventory, projections, native identities,
discovery sources, tag templates, dependency edges, groups, and release-record
policy. It contains no versions, pending intents, remote, clock, or process
settings. Do not hand-edit it or use it as Intentional configuration.

`intentional model --dry-run` previews `written` or `unchanged` without writing.
`--stdout` prints only canonical model JSON without changing the tree. `--json`
prints an operation result with `operation`, `path`, and `dry_run`; it conflicts
with `--stdout`. Invalid input returns exit `1` and leaves the old model untouched.
Initialization writes the model whenever it writes configuration. Intentional
never runs Toha.

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

When `.intentional/release-model.json` exists, `check` compares its bytes with
an in-memory model built from the working-tree inputs. A difference, invalid
JSON, or another schema reports the error `release-model-stale`; run
`intentional model` to repair it. An absent model produces no diagnostic.
`status` reports the same condition without repairing the file. Use
`intentional status --json` for structured output; `release_model_stale` is a
boolean. A dirty working tree does not itself make a model stale.

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

The first run writes `.intentional/init-plan.yml` when candidates need input
and exits with code `2`. This means explicit decisions are required, not an
operational failure. An empty `inferred-config.release-units` is expected;
do not populate it manually to resolve candidates.

### Resolve every discovered candidate

Every item under `discovery-candidates` requires `resolution.kind`. The first
plan omits `resolution`; add it to each candidate alongside `detector`, `path`,
`projection`, and `tag`. Use these complete resolution mappings:

Create a release unit for a standalone application or library:

```yaml
resolution:
  kind: independent
  release-unit: sample-application
```

Choose a stable logical release-unit id. It need not match the native package
name or directory, does not rename the package, and becomes the id used by
`add --release-unit` and explicit baseline versions.

Attach another manifest to a release unit created by a candidate in this plan:

```yaml
resolution:
  kind: projection
  release-unit: sample-application
  target-candidate: candidate:0000000000000000000000000000000000000000000000000000000000000000
```

Replace the placeholder with the actual `id` of the candidate whose resolution
is `independent` and creates `sample-application`. For a release unit already
in `.intentional/config.yml`, omit `target-candidate`.

Leave an artifact outside the managed inventory:

```yaml
resolution:
  kind: excluded
```

`excluded` accepts neither `release-unit` nor a free-text reason field. It does
not delete the artifact. Both `independent` and `projection` require
`release-unit`. Every path must be resolved, even when native identities match.
Do not collapse duplicate candidates or invent cross-ecosystem identity.

### Example: a root Go module

This excerpt shows where to insert the resolution. Preserve the actual
candidate id, evidence, native identity, and generated suggestions; do not
replace the full plan with this excerpt:

```yaml
discovery-candidates:
  - id: candidate:0000000000000000000000000000000000000000000000000000000000000000
    detector: go-module
    path: go.mod
    # Keep the generated evidence and native-identity fields here.
    projection:
      adapter: go
      path: go.mod
      mode: none
    tag:
      id: primary
      role: primary
      template: '{id}@{version}'
    resolution:
      kind: independent
      release-unit: sample-application
```

`projection.mode` controls version materialization, not candidate ownership.
It does not satisfy `resolution.kind`. Go uses mode `none`; `go.mod` has no
application version field, so the release version comes from Git tags.

### Apply and verify initialization

Edit only schema-supported resolution fields. Keep fingerprints, evidence,
extraction diagnostics, inferred configuration, suggestions, and state intact.
Rerun the same command, optionally previewing it first:

```bash
intentional init --dry-run
intentional init
```

Intentional validates the complete candidate graph before writing
`.intentional/config.yml`, creating `.intentional/intents/`, and removing the
transient plan. Review release-unit ids, projection modes, and tag templates,
then commit the configuration through the authorized surrounding workflow.
Initialization itself creates neither commits nor tags.

Run `intentional status` and `intentional check` after configuration is ready.
If a missing baseline is reported, use the baseline procedure below before
expecting `check` to pass. Init also scans configured repositories; matching
receipts make reruns no-ops, while new candidates or changed excluded evidence
reopen the resolution workflow.

### Adopt a repository that already released a version

Existing releases do not require Changesets takeover unless Changesets is
actually configured. Resolve candidates normally and review the generated
configuration before committing. Inspect local tags and available history.
The default primary tag template is `{id}@{version}`. If releases use bare
versions, change the generated configuration's template to `{version}`:

```yaml
release-units:
  sample-application:
    # Keep the generated path, projections, and discovery receipts.
    tags:
      primary:
        role: primary
        template: '{version}'
```

Preserve the rest of the configuration; this is an excerpt. Edit the resulting
configuration after init, not the generated candidate suggestion. Commit it
once the template matches the existing stream. `{version}` matches `0.1.0`;
`{id}@{version}` selects a different stream.

Lightweight tags and annotated tags with no tag record fields are
accepted as pre-Intentional history. An existing matching `0.1.0` tag supplies
current version `0.1.0`, including for a Go module. Do not recreate that tag,
move it, create a replacement baseline, or author an intent to repeat the
release. `tag --baseline` preserves it and ignores an explicit `--version` for
that existing stream. Tags with any tag record fields
must pass full tag record validation.

Verify with `status` and `check`. If an expected baseline appears missing,
inspect the template and locally available release history before creating a
new tag stream. Once validation succeeds, author intents for subsequent
changes. Existing build and publication automation remains owned by the
repository's release workflow.

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

After that commit, create annotated tag records against the same saved
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
local until the authorized harness pushes them. Repeating the same tag command
validates exact existing tag records and creates only missing selected tag
records, so dry-run verification and recovery after partial or complete tag
creation are safe. Any tag record mismatch fails closed.

After every planned tag exists, verify the Release from any of its tags:

```bash
intentional verify <tag> --json
```

`verify` needs Git history only. Exit `0` means every check passed; report any
`producing-version-differs` advisory with the result. Exit `1` names the failed
check in a diagnostic code; investigate it before any publication step relies
on the Release.

## Establish baseline tags

Use baselines only when a configured tag stream has no matching existing tag.
Commit configuration first and inspect existing tags before creating one.
Matching pre-Intentional tags already supply authority and remain unchanged.

When version-bearing projections all agree, preview and create a baseline:

```bash
intentional tag --baseline --dry-run
intentional tag --baseline
```

If there is no version-bearing projection, including for Go modules, an
explicit version is required. For an already released application's current
version of `0.1.0`:

```bash
intentional tag --baseline --version sample-application=0.1.0 --dry-run
intentional tag --baseline --version sample-application=0.1.0
```

Use the actual release-unit id. A new unreleased repository can start from an
explicit `0.0.0` baseline. Named workspace tag streams without matching tags
need `--version workspace/release=X.Y.Z`. Repeat `--version` for multiple
streams. Version-bearing projections must agree with an explicit baseline;
an override cannot hide drift.

Baselines create local annotated tags at the current commit, not at an earlier
release commit. Inspect dry-run targets and consider the repository's
publication triggers before the surrounding workflow pushes tags. A baseline
does not itself publish a release. Finish with `status` and `check`, then
verify the baseline from any tag it created with `intentional verify <tag>
--json`. Do not combine `--baseline` with `--channel` or `--plan`.

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
