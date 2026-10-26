---
title: Configuration
order: 5
product: intentional
relationships:
  implements: intent-driven-polyglot-release
---

Intentional keeps its durable working state in `.intentional/` at the
workspace root:

| Path | Purpose |
| --- | --- |
| `.intentional/config.yml` | Release-unit inventory and interpretation contract. |
| `.intentional/intents/` | Pending change-intent Markdown files. |
| `.intentional/init-plan.yml` | Transient discovery or adoption decisions. |

Configuration uses kebab-case YAML keys and rejects unknown fields. `init`
produces explicit candidates before it writes canonical configuration.

## Configuration file

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
  sample-library:
    path: packages/library
    projections:
      - adapter: npm
        file: package.json
        mode: committed
      - adapter: pub
        file: pubspec.yaml
        mode: committed
    tags:
      primary:
        role: primary
        template: "{id}@{version}"
      pub:
        role: projection
        template: "sample-library-pub@{version}"
        require-phase: before-publication
  sample-application:
    path: packages/application
    depends-on: [ sample-library ]
    projections:
      - adapter: npm
        file: package.json
        mode: committed
    tags:
      primary:
        role: primary
        template: "{id}@{version}"
```

### Top-level model

| Key | Purpose |
| --- | --- |
| `$schema` | Schema URL for editor validation. |
| `contract` | Versioned interpretation semantics used by plans and tags. |
| `settings` | Workspace-wide bump behavior. |
| `release-units` | Logical version and changelog inventory. |
| `workspace-tags` | Repository-level release records and triggers. |
| `fixed` | Groups whose managed members release at one shared version. |
| `linked` | Groups whose releasing members share a version calculation. |
| `discovery` | Managed and excluded candidate receipts. |

### Settings

| Key | Values | Meaning |
| --- | --- | --- |
| `internal-dependency-bump` | `major`, `minor`, `patch` | Minimum bump propagated to internal dependents that share an ecosystem adapter with the dependency. |
| `pre-1-0-bump-mapping` | `component`, `compatibility` | Interpretation of bump names before 1.0.0. |

New workspaces use `compatibility`: before 1.0.0, `major` advances the minor
component while `minor` and `patch` advance the patch component. `component`
always applies the named Semantic Versioning component directly.

### Release units

`release-units` is keyed by stable logical ids. An id need not match an
ecosystem package name.

| Key | Required | Meaning |
| --- | --- | --- |
| `path` | Yes | Release-unit directory relative to the workspace root. |
| `projections` | No | Files in which a version may be materialized. |
| `tags` | Yes | Exactly one primary tag and optional projection tags. |
| `depends-on` | No | Internal release-unit dependencies. |
| `disposition` | No | `managed` by default; `suspended` keeps identity while blocking a required release. |

Each projection has an adapter, a file, and a mode. Generic `json`, `toml`,
and `yaml` projections also require a pointer to the version field. A `json`
pointer selects an array element only by its index without leading zeros, such
as `/versions/1`.

A `file` is relative to the release-unit path. It must not contain `..` or a
segment that starts with a drive prefix such as `C:`. A `file` that starts
with `/` is a root file: it is relative to the repository root, so that a
release unit can write its version into a file outside its own path. Only
generic projections may use a root file. A root file has one form: one `/`,
then segments separated by `/`, with no empty, `.`, or `..` segment, no
segment that starts with a drive prefix, and no `\`. A file that resolves
outside the repository through a symlink is refused. The release unit's
changelog stays at the release-unit path.

| Mode | Behavior |
| --- | --- |
| `committed` | `apply` writes the release version. |
| `injected` | `stamp` writes a computed build version. |
| `none` | The listed manifest is not written; tags remain authoritative. |

The ecosystem adapters are `npm`, `cargo`, `pub`, `python`, `msbuild`, and
`go`. The generic adapters are `json`, `toml`, and `yaml`.

### Shared projection files

Several release units may write different fields of one file. `apply` edits
the file once, with the field of each released unit, and leaves the fields of
the other units as they are.

```yaml
release-units:
  sample-cli:
    path: packages/cli
    projections:
      - adapter: npm
        file: package.json
        mode: committed
      - adapter: yaml
        file: /docs/site.yml
        pointer: /products/sample/version
        mode: committed
  sample-docs:
    path: docs/sample
    projections:
      - adapter: yaml
        file: /docs/site.yml
        pointer: /products/sample/docs-version
        mode: committed
```

Two projections must not write the same field of one file. Configuration load
refuses them, in one release unit or across release units:

- Files compare by their path from the repository root, so `/docs/site.yml` on
  any unit and `docs/site.yml` on a unit at `.` are one file.
- Fields compare by their parsed pointer, so `products.sample.version` and
  `/products/sample/version` are one field. An ecosystem adapter owns its
  fixed version field: an `npm` projection and a `json` projection at
  `/version` in one `package.json` collide.
- Projections with mode `none` do not count, except `go` projections: a major
  release rewrites the module path in every mode.

`status`, `check`, `apply`, `stamp`, and `tag` also refuse two projections that
reach one field of one file through different paths: a symlink, a hard link, or
letter case on a filesystem that ignores case. Through such paths, `apply` and
`stamp` still edit one file once, including a file that does not exist yet,
such as a first changelog. When Intentional cannot tell whether a directory
ignores case, it refuses two new files whose paths differ only in letter case.

A `cargo` member whose version is inherited writes `/workspace/package/version`
in its workspace manifest. Several such members may share that field, and
`apply` refuses them when their versions differ, but no other projection may
write it.

A generic projection has no ecosystem, so a release of `sample-cli` does not
bump `sample-docs`. Put both units in a `fixed` or `linked` group when they
must release together.

### Dependency propagation

When a release unit releases, each unit that lists it in `depends-on` receives
at least the `internal-dependency-bump`, but only when both units have a
projection with the same ecosystem adapter. In that case the dependent's
manifest holds a version range on the dependency, and the release rewrites it,
so the dependent changes and must release too. A `depends-on` edge between
units without a shared ecosystem adapter, including any unit that projects
only through generic adapters, orders releases and carries no bump. To make
units in different ecosystems release together, put them in a `fixed` or
`linked` group.

### Tags, phases, and order

Each release unit has one tag with `role: primary` and may have tags with
`role: projection`. Named workspace tags live under `workspace-tags`. Every
template contains `{version}`, may contain `{id}` where supported, and must not
prefix the version with `v`.

Common templates are:

- `{id}@{version}` for a release-unit namespace, such as
  `sample-library@1.4.0`;
- `{version}` for a single-release-unit repository or workspace release record.

Set `require-phase` to `before-publication` or `after-publication` when the
executor must declare the external phase before creating a tag. Set
`tag-after` to tag ids that must already exist and agree with the planned
commit, version, contract, and digest. Intentional rejects cycles and emits the
resulting order in the canonical release plan.

## Intent files

An intent filename stem is its stable id. YAML frontmatter maps release-unit
ids to `major`, `minor`, or `patch`; the body is changelog prose:

```markdown
---
sample-library: minor
sample-application: patch
---

Add a user-visible capability.
```

Intentional rejects unknown release units, invalid bumps, and empty prose.

## Discovery and initialization

`init` recognizes these manifests:

| Manifest | Adapter | Default mode |
| --- | --- | --- |
| `package.json` | `npm` | `committed` |
| package-bearing `Cargo.toml` | `cargo` | `committed` |
| `pubspec.yaml` | `pub` | `committed` |
| `pyproject.toml` | `python` | `committed` |
| `*.csproj` | `msbuild` | `committed` |
| `go.mod` | `go` | `none` |
| `devcontainer-feature.json` | `json` | `committed` |
| `devcontainer-template.json` | `json` | `committed` |

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

## Version interpretation and release groups

`settings.pre-1-0-bump-mapping` selects one of two contracts:

- `component`: `major`, `minor`, and `patch` increment the corresponding
  SemVer component. This matches Changesets and is recommended during adoption.
- `compatibility`: before 1.0, `major` advances `0.x.y` to `0.(x+1).0`, while
  `minor` and `patch` advance to `0.x.(y+1)`. New repositories use this mapping.

At and after 1.0, both mappings use ordinary SemVer component increments.

```yaml
fixed:
  - [ library, application ]
linked:
  - [ service, utility ]
```

When any fixed member receives a direct or dependency-propagated bump, every
member releases at one version derived from the group's highest current version
and highest requested bump. A suspended fixed member blocks the shared release.

Linked groups release only affected members. Releasing members use the highest
current version across the whole group and the highest bump among affected
members; unaffected members retain their current versions. A suspended linked
member blocks only when it would release. A release unit belongs to at most one
fixed or linked group.

When init reconciles an existing configuration, current non-development npm
manifest dependencies own edges between npm release units. Removing such a
manifest dependency removes its `depends-on` edge. Configured edges to release
units without an npm projection remain authoritative because an npm manifest
cannot represent those cross-ecosystem or non-native ordering relationships.
