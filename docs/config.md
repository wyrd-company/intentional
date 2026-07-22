---
docs: true
title: Configuration
order: 4
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
| `internal-dependency-bump` | `major`, `minor`, `patch` | Minimum bump propagated to internal dependents. |
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

Each projection has an adapter, relative file path, and mode. Generic `json`,
`toml`, and `yaml` projections also require a pointer to the version field.

| Mode | Behavior |
| --- | --- |
| `committed` | `apply` writes the release version. |
| `injected` | `stamp` writes a computed build version. |
| `none` | The listed manifest is not written; tags remain authoritative. |

The ecosystem adapters are `npm`, `cargo`, `pub`, `python`, `msbuild`, and
`go`. The generic adapters are `json`, `toml`, and `yaml`.

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

Each candidate contains source evidence, extracted identity and version when
available, and only the projection or tag suggestions supported by that
evidence. Set its `resolution` to `independent`, `projection`, or `excluded`,
then rerun `init`. Managed and excluded receipts let later runs distinguish
unchanged evidence from a manifest that needs a new decision.
