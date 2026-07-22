---
relationships:
  implements: intent-driven-polyglot-release
---

# intentional

`intentional` is an intent-driven polyglot release and versioning CLI. Pending
intent files determine what changes, annotated Git tags record what was
released, and manifests are format-preserving projections of that state.

The tool supports npm, Cargo, Pub, PEP 621 Python projects, MSBuild projects,
Go modules, Dev Container Features and Templates, and arbitrary JSON, TOML,
and YAML version fields. It writes release state in the working tree and
creates annotated tags. The surrounding harness owns commits, pushes,
publication, registry observation, and forge operations. Generated tag
templates do not prefix versions with `v`.

## Install

With Homebrew:

```console
brew tap wyrd-company/tools
brew install intentional
```

With Cargo:

```console
cargo install intentional-cli --locked
```

The minimum supported Rust version is 1.82. The library crate is
`intentional-core`; the binary package is `intentional-cli` and installs the
`intentional` executable.

## Initialize a repository

```console
intentional init
```

Initialization requires a Git repository, whose tags provide release authority
and whose ignore rules bound discovery. It uses package-manager workspace
membership and manifest-native package names. Repeated directory basenames do
not collide because directories are not package identities. `--scan-all`
explicitly includes supported manifests outside the declared workspace. Both
modes honor Git ignore rules and always skip version-control metadata,
Intentional state, and ecosystem caches; ordinary names such as `build`,
`dist`, `bin`, and `vendor` are scanned unless the repository ignores them.

Every newly discovered npm, Cargo, Go, Python, MSBuild, Dart, Dev Container
Feature, or Dev Container Template manifest first appears in
`.intentional/init-plan.yml` as an unresolved candidate. Choose `independent`,
`projection`, or `excluded`, then rerun `intentional init` to write
configuration only after the complete candidate graph validates. New
configurations use the `compatibility` pre-1.0 bump mapping.

```yaml
$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
settings:
  internal-dependency-bump: patch
  pre-1-0-bump-mapping: compatibility
workspace-tags:
  release:
    template: "{version}"
release-units:
  library:
    path: packages/library
    projections:
      - adapter: npm
        file: package.json
        mode: committed
      - adapter: pub
        file: pub/library/pubspec.yaml
        mode: committed
    tags:
      primary:
        role: primary
        template: "{id}@{version}"
      pub:
        role: projection
        template: "library-pub@{version}"
        require-phase: before-publication
  application:
    path: packages/application
    depends-on: [library]
    projections:
      - adapter: npm
        file: package.json
        mode: committed
    tags:
      primary:
        role: primary
        template: "{id}@{version}"
```

Every release unit has exactly one primary tag and any number of projection
tags. All tags for one logical release carry the same version, target commit,
interpretation contract, and plan digest. Named workspace tags provide
repository-level records and continuous-delivery triggers without becoming a
release unit's version authority.

When init reconciles an existing configuration, current non-development npm
manifest dependencies own edges between npm release units. Removing such a
manifest dependency removes its `depends-on` edge. Configured edges to release
units without an npm projection remain authoritative because an npm manifest
cannot represent those cross-ecosystem or non-native ordering relationships.

Projection modes are:

- `committed`: `apply` writes the release version.
- `injected`: `stamp` writes the computed build version.
- `none`: no manifest version is written; Go uses this mode.

Generic `json`, `toml`, and `yaml` projections also require a `pointer`, such
as `/metadata/version`. A release unit with no version-bearing projection is
tag-only.

### Discovery candidates and receipts

The shared detector contract uses one artifact-neutral candidate shape.
Candidate ids are stable hashes of the detector id and exact workspace-relative
path. File digests, native identity, and raw version text remain initialization
evidence. Projection and tag suggestions appear only when extraction supplies
the fields they require. Extraction diagnostics report unreadable or missing
fields but do not claim that an artifact is publishable.

The `devcontainer-feature.json` and `devcontainer-template.json` detectors are
distinct. Each reads only the top-level `id` and `version`, suggests the `id`
as native identity, and exposes a committed JSON projection at `/version` when
the version is Semantic Versioning 2.0.0. Missing, unreadable, or unsupported
identity/version evidence produces an extraction diagnostic. These detectors
do not inspect companion files such as `install.sh` or `devcontainer.json`, nor
do they inspect workflows, OCI registries, publication state, or overall
artifact correctness.

An initialization-plan candidate has this shape (shown as an excerpt):

```yaml
discovery-candidates:
  - id: candidate:d888bb15d92b6478bbb66e9f2a01a12da7113e30d3a59440365a25e8f317ce0f
    detector: sample-manifest
    path: packages/library/manifest.json
    evidence:
      - path: packages/library/manifest.json
        digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
    native-identity: library
    raw-version:
      value: "1.2.3"
      evidence:
        - path: packages/library/manifest.json
          digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
    projection:
      adapter: json
      path: packages/library/manifest.json
      mode: committed
      pointer: /version
    tag:
      id: primary
      role: primary
      template: "{id}@{version}"
    resolution:
      kind: independent
      release-unit: library
```

A resolution is `independent`, `projection`, or `excluded`. Independent and
projection resolutions name the final release-unit id. A projection names
`target-candidate` when it follows an independent candidate in the same plan;
without that field it targets an already configured release unit. Initialization
rejects duplicate release-unit creators, absent targets, identity disagreement,
and projection cycles.

Applied choices leave generic discovery receipts in canonical configuration:

```yaml
discovery:
  managed-paths:
    - detector: sample-manifest
      path: packages/library/manifest.json
      release-unit: library
  excluded-paths:
    - detector: sample-manifest
      path: examples/example/manifest.json
      evidence-digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
```

Managed receipts preserve the chosen release-unit relationship. Excluded
receipts bind the exact detector/path identity to its evidence digest, so
changed evidence requires a new resolution rather than silently inheriting an
old exclusion. Paths are literal workspace-relative paths, not globs. Init
scans configured repositories too: matching receipts make reruns no-ops, while
new or changed excluded paths reopen the same candidate workflow.

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
Changesets' range-conditional peer-dependent option is one such explicit
contract choice because Intentional applies configured `depends-on` edges
uniformly; any pending-release difference remains a parity blocker.
Internal `devDependencies` participate in the Changesets source computation but
are not authored as durable Intentional `depends-on` edges; adopting that
contract is also explicit, and current divergence remains blocking.

`--take-over` is the only authority handoff. It refuses unresolved, stale, or
non-equivalent plans, then performs a rollback-capable transaction that writes
canonical Intentional state, moves pending intents, removes recognized
`.changeset/` state, and consumes the one transient initialization plan.
References in workflows, package scripts and dependencies, lockfiles, release
scripts, and tests are reported with exact locations; Intentional verifies the
edits but does not rewrite those files.

After committing the takeover changes, establish canonical tag authority:

```console
intentional tag --baseline
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

## Version interpretation and release groups

`settings.pre-1-0-bump-mapping` selects one of two contracts:

- `component`: `major`, `minor`, and `patch` increment the corresponding
  SemVer component. This matches Changesets and is recommended during adoption.
- `compatibility`: before 1.0, `major` advances `0.x.y` to `0.(x+1).0`, while
  `minor` and `patch` advance to `0.x.(y+1)`. New repositories use this mapping.

At and after 1.0, both mappings use ordinary SemVer component increments.

```yaml
fixed:
  - [library, application]
linked:
  - [service, utility]
```

When any fixed member receives a direct or dependency-propagated bump, every
member releases at one version derived from the group's highest current version
and highest requested bump. A suspended fixed member blocks the shared release.

Linked groups release only affected members. Releasing members use the highest
current version across the whole group and the highest bump among affected
members; unaffected members retain their current versions. A suspended linked
member blocks only when it would release. A release unit belongs to at most one
fixed or linked group.

## Author and inspect intents

```console
intentional add \
  --release-unit library:minor \
  --release-unit application:patch \
  --message "Add a useful capability."

intentional status
intentional check
```

Without flags, `add` prompts for a release unit, bump, and changelog message. Intent
frontmatter maps release-unit ids to bumps, and the Markdown body supplies
release notes.

`status` lists pending intents, tag-derived current versions, computed next
versions, suspended release blockers, missing baselines, and manifest drift.
`check` validates configuration and intents, baseline presence, and deterministic
release planning.

## Plan, apply, and tag

```console
intentional plan > release-plan.json
intentional apply

git add -A
git commit -m "chore: apply release"

intentional tag --plan release-plan.json
```

`plan` writes compact canonical JSON sealed by SHA-256. The sealed payload
contains the interpretation contract, generator identity, release units, old
and new versions, contributing intent ids, rendered release notes, release-unit
and workspace tags, phase declarations, `tag-after` prerequisites, and tag
order. It contains no publication graph.

`apply` updates committed projections, internal dependency ranges, and
changelogs, then consumes final-release intents. It changes only the working
tree. `tag` verifies the supplied sealed plan against the materialized tree and
creates annotated records at the release commit. Without `--plan`, a final
release is recovered from the intents deleted by the release commit; a channel
release is recovered from its retained intents.

A tag with `require-phase` is created only by an equal executor declaration:

```console
intentional tag --plan release-plan.json --phase before-publication
intentional tag --plan release-plan.json --phase after-publication
```

The declaration does not assert or verify publication. `tag-after` expresses
only observable tag prerequisites; Intentional verifies prerequisite tag
records before creating dependent tags. External continuous delivery owns all
publication sequencing and registry evidence.

## Stamp build versions

For an `injected` projection, stamp the intent-derived version without touching
changelogs or intents:

```console
intentional stamp
intentional stamp --prerelease alpha
```

The prerelease form composes the next intent version with first-parent commit
height since the latest matching primary tag, for example `1.3.0-alpha.5`.

## Channel releases

Channel state derives from tags:

```console
intentional plan --channel beta > release-plan.json
intentional apply --channel beta
git add -A
git commit -m "chore: apply beta release"
intentional tag --channel beta --plan release-plan.json
```

The first release is `X.Y.Z-beta.1`; each iteration advances from existing tag
records. Channel applies retain intents. A later final `apply` replaces channel
changelog sections with the consolidated final section.

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

## Development checks

```console
task build
task fmt-check
task lint
task test
task ci
```
