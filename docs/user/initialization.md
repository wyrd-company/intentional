---
title: Initialization
order: 3
product: intentional
relationships:
  implements: intent-driven-polyglot-release
---

Initialize once to choose which artifacts Intentional manages and which Git
tags define their current versions. A repository that already released a
version uses the same discovery workflow as a new repository. Changesets
repositories also require the explicit takeover described below.

## Discover the inventory

Run from the Git repository root:

```console
intentional init
```

Discovery recursively follows Git ignore rules and hard cache exclusions.
Package-manager workspace membership informs recommendations, dependency
analysis, and Changesets parity; it never limits discovery. Version-control
metadata, Intentional state, and ecosystem caches are skipped. Ordinary names
such as `build`, `dist`, `bin`, and `vendor` are scanned unless ignored.

The first run writes `.intentional/init-plan.yml` when candidates need input
and exits with code `2`. This is a request for decisions, not an operational
failure. `inferred-config.release-units: {}` is expected before candidates
have been resolved. Do not fill that mapping by hand to resolve candidates.

## Resolve every candidate

Every item under `discovery-candidates` needs a `resolution.kind`. The first
plan omits `resolution`; add it inside each candidate, alongside `detector`,
`path`, `projection`, and `tag`. Use kebab-case keys.

Choose a stable release-unit id, such as `sample-application`. This is the
logical name used in configuration and later `add --release-unit` commands.
It need not match the package name or directory, and choosing it does not
rename the native package or module.

### Create a release unit

For a standalone application or library, append this mapping to its candidate:

```yaml
resolution:
  kind: independent
  release-unit: sample-application
```

The following excerpt shows the insertion point for a root Go module. Keep the
actual generated id, evidence, native identity, and suggestions; the omitted
fields here are only to keep the excerpt short:

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

`resolution.kind: independent` creates a release unit. `projection.mode` is a
separate field that controls version materialization; it does not resolve the
candidate. For Go it is `none`: `go.mod` has no application version field,
and Git tags supply the release version.

### Attach another manifest to that unit

Use `projection` when two manifests represent the same logical release. Both
`independent` and `projection` require `release-unit`. If the owning unit is
created by another candidate in this plan, copy that creator's actual `id`
into `target-candidate`:

```yaml
resolution:
  kind: projection
  release-unit: sample-application
  target-candidate: candidate:0000000000000000000000000000000000000000000000000000000000000000
```

If the unit already exists in `.intentional/config.yml`, omit
`target-candidate`. Do not copy the placeholder id from this example.

### Leave an artifact unmanaged

Use this complete mapping for an artifact outside the managed inventory:

```yaml
resolution:
  kind: excluded
```

This does not delete the artifact. No `release-unit` or free-text reason field
is accepted for this resolution.

Resolve every path, including duplicate native identities. Do not merge or
delete candidates. Initialization rejects duplicate release-unit creators,
absent targets, identity disagreement, and projection cycles. See the
[configuration reference](https://wyrd.foo/docs/intentional/config) for candidate
evidence, extraction diagnostics, and discovery receipts.

## Apply the initialization plan

Edit only candidate and diagnostic resolution fields supported by the plan
schema; retain generated evidence, fingerprints, suggestions, and state.
Rerun the same initialization command after resolving candidates:

```console
intentional init --dry-run
intentional init
```

Ordinary initialization writes `.intentional/config.yml`, creates
`.intentional/intents/`, and removes `.intentional/init-plan.yml` when the
complete candidate graph validates. Review the generated configuration,
including release-unit ids and tag templates, then commit it through your
normal repository workflow. Init does not commit, publish, or create tags.

A later `init` scans configured repositories too. Matching receipts make it a
no-op; new candidates or changed excluded evidence require explicit input.

## Bootstrap an already released repository

Suppose the repository has already released `0.1.0`. Resolve its candidates and
review the generated configuration before committing it. The default primary
tag template is `{id}@{version}`. If existing releases use bare versions,
change the resulting `.intentional/config.yml` to use `{version}` instead:

```yaml
release-units:
  sample-application:
    # Keep the generated path, projections, and discovery receipts.
    tags:
      primary:
        role: primary
        template: '{version}'
```

This is a configuration excerpt, not a replacement file. Make this edit after
init writes the configuration, rather than changing generated candidate
suggestions. Commit the configuration once it matches the existing tag stream.
`{version}` matches `0.1.0`; `{id}@{version}` describes a different stream.

Ensure the release tags and their history are available in the local checkout,
then inspect:

```console
intentional status
intentional check
```

Matching pre-Intentional lightweight tags and annotated tags without
Intentional record fields supply current-version authority. With a matching
`0.1.0` tag, the current version is `0.1.0`: no new baseline tag or repeat release
is needed. Do not recreate or move that tag. `tag --baseline` also leaves it in
place, ignoring an explicit `--version` for that existing stream. A tag carrying
any Intentional record field is validated in full;
partial or corrupt records fail validation.

For Go this works without a manifest version. If the expected tag is present
but `status` reports a missing baseline, inspect the configured template and
available history before creating a new tag stream.

After validation succeeds, author intents for subsequent changes. Adoption
itself does not require an intent that re-releases the existing version.
Intentional does not replace the repository's build and publication workflow;
that workflow still owns publishing when release tags are pushed.

## Establish a missing baseline

Use this only when the configured tag stream has no existing matching tag.
Commit configuration first. When version-bearing projections all agree,
preview and create a baseline from them:

```console
intentional tag --baseline --dry-run
intentional tag --baseline
```

A tag-only unit, including a Go module, cannot infer a version from its
manifest. Supply the intended current version explicitly. For an already
released application whose current version is `0.1.0`:

```console
intentional tag --baseline --version sample-application=0.1.0 --dry-run
intentional tag --baseline --version sample-application=0.1.0
```

A new, unreleased repository can use an explicit `0.0.0` baseline. This records
a starting version; it does not publish a release. Named workspace tag streams
without an existing matching tag also need explicit versions, using
`--version workspace/release=0.1.0`. Repeat `--version` for multiple streams.
Do not combine `--baseline` with `--plan` or `--channel`.

Baselines create local annotated tags at the current commit. Preview their
targets before creating them; the surrounding workflow owns pushing tags and
any automation that push triggers. Finish with `intentional status` and
`intentional check` to verify version authority.

## Adopt a Changesets repository

When `.changeset/config.json` exists, ordinary `intentional init` preserves
Changesets authority and writes `.intentional/init-plan.yml`. The plan contains
source fingerprints, inferred configuration, stable diagnostics, exact source
evidence, finite choices, recommendations, editable `resolution` fields,
planned operations, pending-intent conversions, and a parity comparison for
each release unit.

```console
intentional init
# edit only the plan's resolution fields and make the reported repository edits
intentional init
intentional init --take-over --dry-run
intentional init --take-over
```

Initialization exits with:

- `0` for a complete ordinary initialization, a ready adoption plan, or a
  completed takeover.
- `2` when the plan needs resolutions or verified repository edits.
- `1` for invalid input or operational failure.

`intentional init --json` emits the same plan as stable structured JSON.
Reruns preserve resolutions while their evidence remains valid, report stale
resolutions, and verify repository-integration edits from actual file state.
Initialization never proposes a root projection file, and a rerun keeps the
root projections that the configuration already has.

When an npm manifest is explicitly projected onto a non-npm release unit,
initialization may present a `retain` or `remove` integration choice. A
best-effort removal recommendation uses only manifest structure, workspace and
dependency graphs, exact source-tool identity references, and repository use.
The diagnostic separates supporting evidence from contradictory evidence and
states its uncertainty. Descriptions, comments, prose, semantic keywords,
private status, minimal shape, path proximity, and matching versions never
establish removability by themselves. The selected resolution is authoritative;
takeover deletes the manifest only after an explicit `remove` resolution and a
fresh check that no non-source-tool repository references remain.

Changesets `ignore` entries become explicit `suspended`, `excluded`, or
`managed` choices. `suspended` preserves release-unit identity while blocking any
release that requires it, so it is the default recommendation unless repository
evidence supports another disposition.

Pending Changesets Markdown bodies and package bumps are converted losslessly.
Fixed and linked groups, npm dependency ranges and peer edges, internal
dependency policy, and component-mapped pre-1.0 versions participate in the
parity comparison. Private-package version and tag settings are recorded
explicitly because Intentional separates version management from publication
privacy and annotates every managed release. Unsupported or repository-specific
behavior remains an explicit diagnostic.
Repository release metadata and release scripts are retained as external
evidence; Intentional does not interpret their private schemas or infer package,
tag, publication, or ordering semantics from them. Cross-projection identity is
provided only through explicit discovery-candidate resolutions.
When a Changesets `ignore` entry names a native package that a candidate
resolution projects onto another managed release unit, source parity is
undefined at the package boundary. The target remains in both inventories and
takeover stays blocked until the ignore entry is removed or the candidate
resolution is revised.
Changesets' range-conditional peer-dependent option is one such explicit
contract choice because Intentional applies configured `depends-on` edges
uniformly; any pending-release difference remains a parity blocker.
Internal `devDependencies` participate in the Changesets source computation but
are not authored as durable Intentional `depends-on` edges; adopting that
contract is also explicit, and current divergence remains blocking.

`--take-over` is the only authority handoff. It refuses unresolved, stale, or
non-equivalent plans, then performs a rollback-capable transaction that writes
canonical Intentional state, moves pending intents, removes recognized
`.changeset/` state, applies explicitly authorized proxy-manifest removals, and
consumes the one transient initialization plan.
References in workflows, package scripts and dependencies, lockfiles, release
scripts, and tests are reported with exact locations; Intentional verifies the
edits but does not rewrite those files.

After committing the takeover changes, inspect version authority and establish
any missing baselines:

```console
intentional status
intentional tag --baseline --dry-run
intentional tag --baseline
intentional check
```

Baseline inference succeeds only when every version-bearing projection of a
release unit agrees. Tag-only release units and new workspace tag streams require
explicit versions:

```console
intentional tag --baseline \
  --version library=1.2.3 \
  --version workspace/release=1.2.3
```

`status` and `check` report the missing-baseline diagnostic and command until
the configured primary baselines exist.
