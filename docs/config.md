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
| `.intentional/executor-init-plan.yml` | Durable GitHub executor and publication decisions. |

Configuration uses kebab-case YAML keys and rejects unknown fields. `init`
produces explicit candidates before it writes canonical configuration.

## Configuration file

```yaml
$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
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
| `github` | GitHub executor opt-in, workflow paths, gates, and reserved namespaces. |

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

A repository using the GitHub executor declares exactly one workspace tag
without `require-phase`. That tag is the global release tag: the release
workflow publishes it with the release commit, and the publish workflow is
triggered by the name it renders. Every other tag, workspace or release-unit,
declares the phase that creates it.

The rule is stated over workspace tags because a release plan seals the
workspace tags together with the tags of the release units that release. A
release-unit tag without `require-phase` is therefore present in some releases
and absent from others, so it cannot be the tag that triggers publication.
`intentional executor check` reports one, naming it, and a multi-release-unit
repository adopting the executor moves its global release tag under
`workspace-tags` rather than leaving it on a release unit.

Set `require-phase` to `before-publication` or `after-publication` when the
executor must declare the external phase before creating a tag. Set
`tag-after` to tag ids that must already exist and agree with the planned
commit, version, contract, and digest. Intentional rejects cycles and emits the
resulting order in the canonical release plan.

## GitHub executor

The optional top-level `github` property opts the repository into the GitHub
executor. It identifies the repository-owned workflows that carry
Intentional-managed slices and the repository-owned jobs that gate each
workflow's authority transition:

```yaml
github:
  prefix: intentional
  workflows:
    release:
      path: .github/workflows/release.yml
      gates: [ candidate_check ]
    publish:
      path: .github/workflows/publish.yml
      gates: [ artifact_check ]
```

| Key | Required | Meaning |
| --- | --- | --- |
| `workflows.release` | Yes | Workflow performing release preparation and the authority transition. |
| `workflows.publish` | Yes | Tag-triggered workflow performing publication and Release closure. |
| `path` | Yes | Exact workspace-relative workflow file used as the command default. |
| `gates` | No | Repository-owned job ids the managed transition depends on. |
| `prefix` | No | Reserved job, step, and environment variable namespaces. |
| `declined-publications` | No | Publication targets explicitly declined during executor initialization. |

A scalar `prefix` normalizes to lower snake case for jobs and steps and to
upper snake case for environment variables. A mapping with `job` and `envvar`
members sets both namespaces independently. Intentional appends the separator
and derives the protected release environment from the job namespace. The
default reserves `intentional_`, `INTENTIONAL_`, and the `intentional-release`
environment. A gate may not use the reserved job namespace.

Intentional's own configuration keeps its two workspace-versioned crates and
npm launcher in one release unit. The unit has three packages and four
publications: `core` at `crates/core` declares `cargo`, `cli` at `crates/cli`
declares `cargo` and `homebrew`, and `launcher` at `npm` declares `npm`. The
Homebrew destination is `wyrd-company/homebrew-tools`. The unphased workspace
tag is `{version}`. The release-unit tag is `intentional@{version}` with
`require-phase: before-publication`.

`intentional executor init` creates or resumes
`.intentional/executor-init-plan.yml`. Set each candidate `resolution` to
`accept` or `decline` and rerun the command; it exits with code `2` while any
candidate is unresolved. Accepted publishers and declined publication targets
are written to configuration, so a fresh clone does not reopen the same choice.
Initialization also reports the repository settings
Intentional never mutates, including the requirement that the repository GitHub
App be a ruleset bypass actor for the default branch and every managed release
tag namespace. `intentional executor check` validates configuration, recipe
selection, native packager configuration, and the locally observable workflow
contract.

## Publication intent

Each release unit may declare named packages. Each package has a
release-unit-relative path and opts into managed publication through its own
publisher properties. Publisher properties are invalid directly on a release
unit and invalid without the top-level `github` property. Native package
metadata never creates publication intent on its own:

```yaml
release-units:
  sample-library:
    path: packages/library
    packages:
      library:
        path: .
        npm:
          additional-targets:
            github: {}
    tags:
      primary:
        role: primary
        template: "{id}@{version}"
  sample-image:
    path: packages/image
    packages:
      image:
        path: .
        oci:
          ghcr: {}
          dockerhub:
            repository: example-org/sample-image
            omit: [ signature ]
    tags:
      primary:
        role: primary
        template: "{id}@{version}"
```

| Publisher | Destination | Notes |
| --- | --- | --- |
| `npm` | npmjs | An empty mapping selects the primary; `additional-targets.github` adds GitHub Packages. |
| `cargo` | Native registry | Defaults to crates.io unless the manifest names one registry. |
| `homebrew` | Tap repository | `repository` is required, so this publisher is configured directly. |
| `aur` | Arch User Repository | Package name comes from native packager configuration; GoReleaser resolves an unnamed entry to the project name and suffixes every name with `-bin`. |
| `rpm`, `apt` | Native packager | Configuration stays in native packager files. Publication is reported until the managed job that uploads GitHub-hosted deliverables to the draft Release exists. |
| `oci` | `dockerhub`, `ghcr` | At least one peer target; there is no implicit primary. Docker Hub requires `repository`. |

Configuration stores GitHub variable and secret **names** through
`token-secret` and `username-var`; it never stores credential values. Omitting
those fields selects the maintained recipe's conventional names and its
trusted-publishing-first behavior.

Each concrete OCI target owns an `omit` list drawn from `sbom`, `provenance`,
and `signature`. A component absent from the target's maintained recipe fails
validation. Release evidence records only the components that were produced.

Applying a ready plan edits `.intentional/config.yml` in place. Only the keys
the plan establishes are rewritten, so comments, key order, formatting, and
defaults you never wrote are preserved everywhere else. The command reports the
edit as a planned operation, and `--dry-run` shows it before anything is
written.

`intentional executor init` offers only the decisions it can apply on its own.
A target that needs repository data no evidence supplies, such as a Homebrew tap
or a Docker Hub repository, and a target that more than one derived capability
could publish, are configured directly instead.

Intentional derives each release unit's publishable capabilities from native
project evidence and selects exactly one maintained recipe per configured
target. A combination with no maintained recipe, or one that matches more than
one, is a configuration error.

An `rpm` or `apt` deliverable is the released package itself, which consumers
resolve from the GitHub Release. The managed job that uploads GitHub-hosted
deliverables to the draft is not derived yet, so configuring either publisher is
reported by `intentional executor diff` and `intentional executor check` rather
than deriving a publisher job that would publish nothing. Homebrew and AUR
publish descriptors into their own repositories and are derived today.

A Rust package with one binary may publish a Homebrew formula alongside its
Cargo publication. The binary identity comes from exactly one `[[bin]].name`,
or from the package name when `src/main.rs` supplies the package binary. The
maintained build creates Linux x86-64 and macOS Arm64 archives and generates the
formula inside the same sealed subject. The publisher copies that formula to
the configured tap and never invokes Cargo. A library or multi-binary package
must not declare Homebrew because it does not determine one formula identity.

A Go release unit publishes through GoReleaser, and its native
`.goreleaser.yaml` is where the rest of the contract lives. `intentional
executor check` reports the members the selected recipes read: `project_name`,
which the release seals as the subject identity every destination resolves; the
distribution pipe each configured publisher promotes, `brews` for Homebrew,
`nfpms` for RPM and APT, and `aur` for the Arch User Repository; and the
`nfpms` format each system-package publisher distributes; and a literal
`aur[].name` for Arch publication, because the maintained recipe does not
interpret GoReleaser templates. The command package is discovered from the same
file's `builds[].main`, from the release-unit root, or from the conventional
`cmd` directories beneath it.

The build job installs a pinned GoReleaser rather than the newest release. These
recipes read what the packager wrote, at paths and under names it decides, so
every one of those is a claim about a particular version and moving it is a
change to the recipe contract.

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

`init` also recognizes these tag-only artifact formats, which carry no version
in their own files and take their version authority from a canonical Git tag:

| Artifact | Detector | Evidence |
| --- | --- | --- |
| GitHub Action | `github-action` | `action.yml` or `action.yaml` |
| Terraform module | `terraform-module` | a directory holding `.tf` files, keyed on the directory |
| Terraform provider | `terraform-provider` | `go.mod` with a direct `require` on a Terraform plugin module |
| Docker/OCI image | `docker-image` | `Dockerfile`, `Dockerfile.*`, or `*.Dockerfile` |

`init` also recognizes packaging boundaries inside a versioning boundary:

| Package | Detector | Evidence |
| --- | --- | --- |
| Go command | `go-command` | searchable Go package directory containing files that declare `package main` |

Search follows Go's recursive `./...` package set. It excludes `vendor`,
`testdata`, directories beginning with `.` or `_`, and packages owned by nested
Go modules.

Each candidate contains source evidence, extracted identity and version when
available, and only the projection or tag suggestions supported by that
evidence. Set its `resolution` to `independent`, `projection`, or `excluded`,
then rerun `init`. Accepted resolutions name both `release-unit` and `package`.
Managed receipts retain that package ownership. Excluded receipts retain the
exact evidence digest, so an unchanged declined command stays declined in a
fresh clone and a changed command returns for a new decision.
