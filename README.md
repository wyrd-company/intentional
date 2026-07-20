---
relationships:
  implements: intent-driven-polyglot-release
---

# itentional

`itentional` is an intent-driven, polyglot release and versioning CLI. Pending
intent files say what the next version should be, Git tags say what has been
released, and manifests are format-preserving projections of that state.

The tool supports npm, Cargo, Pub, PEP 621 Python projects, MSBuild projects,
Go modules, and arbitrary JSON, TOML, and YAML version fields. It never
publishes packages, commits, pushes, or opens pull requests. `tag` is its only
Git-writing command, and it creates lightweight tags without a `v` prefix.

## Build

```console
task build
cargo run --bin itentional -- --help
```

The workspace version is `0.1.0` and the minimum supported Rust version is
1.82. The library crate is `itentional-core`; the binary package is
`itentional-cli` and installs the `itentional` executable.

## Initialize a repository

```console
itentional init
```

`init` scans for `package.json`, package-bearing `Cargo.toml`, `pubspec.yaml`,
`*.csproj`, `pyproject.toml`, and `go.mod` files. It writes a logical package
inventory to `.itentional/config.yml` and creates `.itentional/intents/`.

A package with npm and Pub projections at one shared version can be configured
like this:

```yaml
$schema: https://intentional.foo/schemas/config.yml
settings:
  global-tag: false
  internal-dependency-bump: patch
packages:
  sample-runtime:
    path: packages/runtime
    projections:
      - adapter: npm
        file: package.json
        mode: committed
      - adapter: pub
        file: pubspec.yaml
        mode: committed
    tag: "{id}@{version}"
  sample-application:
    path: packages/application
    projections:
      - adapter: npm
        file: package.json
        mode: committed
    depends-on: [sample-runtime]
```

Projection modes are:

- `committed`: `apply` writes the release version.
- `injected`: `stamp` writes the computed build version.
- `none`: no manifest version is written; Go uses this mode.

Generic `json`, `toml`, and `yaml` projections also require a `pointer`, such
as `/metadata/version`.

## Author and inspect intents

```console
itentional add \
  --package sample-runtime:minor \
  --package sample-application:patch \
  --message "Add a user-visible capability."

itentional status
itentional check
```

Without flags, `add` prompts for a package, bump, and changelog message. It
creates a memorable-slug Markdown file such as:

```markdown
---
sample-runtime: minor
---

Add a user-visible capability.
```

`status` lists pending intents, tag-derived current versions, computed next
versions, and manifest drift. `check` validates the config and intent package
references and confirms that plan generation is deterministic.

## Plan and apply a final release

```console
itentional plan > release-plan.json
itentional apply

# The surrounding harness owns commit choreography.
git add -A
git commit -m "chore: apply release"

itentional tag
```

`plan` writes compact canonical JSON to standard output. Its object keys are
sorted, and its `digest` is a SHA-256 seal over the canonical payload excluding
the seal itself. The plan contains old and new versions, contributing intent
ids, tags, dependency-ordered publication order, and rendered release notes.

`apply` updates committed projections, rewrites internal dependency ranges,
updates each logical package's `CHANGELOG.md`, and deletes consumed intent
files. It changes only the working tree. `tag` then creates the per-package
tags, plus a plain global SemVer tag when `settings.global-tag` is enabled.

## Stamp build versions

For an `injected` projection, stamp the intent-derived version without touching
changelogs or intents:

```console
itentional stamp
itentional stamp --prerelease alpha
```

The prerelease form composes the next intent version with first-parent commit
height since the latest matching package tag, for example
`1.3.0-alpha.5`.

## Channel releases

Channel state is derived entirely from existing tags:

```console
itentional plan --channel beta > release-plan.json
itentional apply --channel beta
git add -A
git commit -m "chore: apply beta release"
itentional tag --channel beta
```

The first release is `X.Y.Z-beta.1`; the next iteration is the highest matching
channel tag plus one. Channel applies retain intents. A later channel-less
`apply` removes prerelease changelog sections for that version and writes the
consolidated final section.

## Dry runs

Every mutating command supports `--dry-run`:

```console
itentional init --dry-run
itentional add --package sample-runtime:patch --message "Correct a defect." --dry-run
itentional apply --dry-run
itentional stamp --prerelease alpha --dry-run
itentional tag --dry-run
```

Dry runs print the same operation list as a real invocation and make no
filesystem or Git changes.

## Development checks

```console
task fmt-check
task lint
task test
task ci
```
