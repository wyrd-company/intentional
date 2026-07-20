---
relationships:
  implements: intent-driven-polyglot-release
---

# intentional

`intentional` is an intent-driven, polyglot release and versioning CLI. Pending
intent files say what the next version should be, Git tags say what has been
released, and manifests are format-preserving projections of that state.

The tool supports npm, Cargo, Pub, PEP 621 Python projects, MSBuild projects,
Go modules, and arbitrary JSON, TOML, and YAML version fields. It never
publishes packages, commits, pushes, or opens pull requests. `tag` is its only
Git-writing command, and it creates lightweight tags without a `v` prefix.

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

## Build

```console
task build
cargo run --bin intentional -- --help
```

The workspace version is `0.1.0` and the minimum supported Rust version is
1.82. The library crate is `intentional-core`; the binary package is
`intentional-cli` and installs the `intentional` executable.

## Initialize a repository

```console
intentional init
```

`init` scans for `package.json`, package-bearing `Cargo.toml`, `pubspec.yaml`,
`*.csproj`, `pyproject.toml`, and `go.mod` files. It writes a logical package
inventory to `.intentional/config.yml` and creates `.intentional/intents/`.

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
intentional add \
  --package sample-runtime:minor \
  --package sample-application:patch \
  --message "Add a user-visible capability."

intentional status
intentional check
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

Before `1.0.0`, bumps express compatibility significance uniformly across
ecosystems: `major` advances `0.x.y` to `0.(x+1).0`, while both `minor` and
`patch` advance it to `0.x.(y+1)`. Reaching `1.0.0` is always an explicit human
act: deliberately tag `1.0.0`, after which the next release computes from that
tag using strict SemVer rules.

## Plan and apply a final release

```console
intentional plan > release-plan.json
intentional apply

# The surrounding harness owns commit choreography.
git add -A
git commit -m "chore: apply release"

intentional tag
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
intentional stamp
intentional stamp --prerelease alpha
```

The prerelease form composes the next intent version with first-parent commit
height since the latest matching package tag, for example
`1.3.0-alpha.5`.

## Channel releases

Channel state is derived entirely from existing tags:

```console
intentional plan --channel beta > release-plan.json
intentional apply --channel beta
git add -A
git commit -m "chore: apply beta release"
intentional tag --channel beta
```

The first release is `X.Y.Z-beta.1`; the next iteration is the highest matching
channel tag plus one. Channel applies retain intents. A later channel-less
`apply` removes prerelease changelog sections for that version and writes the
consolidated final section.

## Dry runs

Every mutating command supports `--dry-run`:

```console
intentional init --dry-run
intentional add --package sample-runtime:patch --message "Correct a defect." --dry-run
intentional apply --dry-run
intentional stamp --prerelease alpha --dry-run
intentional tag --dry-run
```

Dry runs print the same operation list as a real invocation and make no
filesystem or Git changes.

## GitHub Action

The plumbing action downloads a released Linux binary and runs one command.
Pin the action to a plain SemVer tag because intentional tags do not use a `v`
prefix. This example validates the repository, exposes the canonical plan as
an output, and records whether that plan contains any changed packages:

```yaml
jobs:
  release-intent:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Validate release intent
        uses: wyrd-company/intentional@0.1.1

      - name: Build release plan
        id: release-plan
        uses: wyrd-company/intentional@0.1.1
        with:
          command: plan

      - name: Save changed release plan
        if: steps.release-plan.outputs.changed == 'true'
        env:
          PLAN_JSON: ${{ steps.release-plan.outputs.plan }}
        run: printf '%s\n' "$PLAN_JSON" > release-plan.json
```

The action also accepts `status`, `stamp`, `apply`, and `tag`. `channel` applies
to `plan`, `apply`, and `tag`; `prerelease` applies to `stamp`; and `dry-run`
applies to the mutating commands.

## Release pipeline

A release is prepared with `intentional apply`, committed, tagged with
`intentional tag`, and sent to GitHub by pushing the plain SemVer tag. The
tag-triggered release workflow builds platform archives, creates the GitHub
Release, publishes `intentional-core` followed by `intentional-cli` to crates.io,
updates the stable major action tag after 1.0, and publishes the `intentional`
formula to the `wyrd-company/tools` Homebrew tap.

## Development checks

```console
task fmt-check
task lint
task test
task ci
```
