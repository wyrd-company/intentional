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

With npm:

```console
npm install --global @wyrd-company/intentional
```

Or run the scoped package without a global installation:

```console
npx --yes @wyrd-company/intentional --version
```

The npm package downloads the matching binary from an immutable, attested
GitHub Release and verifies its checksum before installation. It supports Linux
x64 and arm64, macOS arm64, and Windows x64. GitHub Releases also publish those
archives and `SHA256SUMS` for direct installation. Other platforms can build
from source with `cargo install --path crates/cli --locked`.

The minimum supported Rust version is 1.85. The minimum supported Git is 2.7.4.
The library crate is `intentional-core`; the binary package is
`intentional-cli` and installs the `intentional` executable.

## Agent workflow

Print the complete agent-facing Intentional skill from the installed binary:

```console
intentional skill
```

The output is a valid `SKILL.md`, suitable for redirecting into an agent's skill
directory. Because the document is compiled into the executable, its workflow
and safety guidance stays aligned with that Intentional version.

## Initialize a repository

```console
intentional init
```

Initialization requires a Git repository, whose tags provide release authority
and whose ignore rules bound discovery. It recursively discovers every
supported manifest in that boundary. Package-manager workspace membership is
evidence for recommendations, dependency analysis, and Changesets parity, not
an inclusion boundary. Discovery always skips version-control metadata,
Intentional state, ecosystem caches, and tool-owned directories that describe
how to work on a repository rather than what it releases, currently
`.devcontainer`; ordinary names such as `build`, `dist`,
`bin`, and `vendor` are scanned unless the repository ignores them.

Every newly discovered npm, Cargo, Go, Python, MSBuild, Dart, Dev Container
Feature, Dev Container Template, GitHub Action, Terraform module, Terraform
provider, or Docker/OCI build definition first appears in
`.intentional/init-plan.yml` as an unresolved candidate. Choose `independent`,
`projection`, or `excluded`, then rerun `intentional init` to write
configuration only after the complete candidate graph validates. New
configurations use the `compatibility` pre-1.0 bump mapping.
Candidates are identified by detector and exact path, so manifests with one
native identity at different paths remain independently resolvable. Native
identity conflicts are rejected only after all explicit resolutions have been
applied.

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

Intentional uses one workspace-root release unit for its shared Cargo and npm
version. Its unphased `{version}` workspace tag is the global release tag each
plan seals, and that tag triggers publication. `intentional@{version}` records
the release unit before publication. The
unit contains three publication packages: `core` at `crates/core` publishes
with Cargo, `cli` at `crates/cli` publishes with Cargo and Homebrew, and
`launcher` at `npm` publishes with npm. The Homebrew route builds Linux x86-64
and Linux Arm64 through the pinned Cross baseline and builds macOS Arm64 with
Cargo. It builds each archive once, seals them with the generated
formula, and promotes that formula to the configured tap without rebuilding
each archive.

A Rust command can use the same sealed archives for system publication. A
Homebrew package provides its tap repository and installs the release GitHub
App there. RPM and APT packages provide a repository-owned composite delivery
Action, public repository and signing-key URLs, an observation deadline, and
their index coordinates. Their Cargo package declares at least one author for
the downstream package maintainer field. An Arch User Repository (AUR) package
declares
`aur: {}` and provides the `INTENTIONAL_AUR_KEY` repository secret. Intentional
derives the command identity, marks the generated `-bin` descriptor as providing
and conflicting with its base package, creates the configured native packages
or descriptors in the aggregate build, and never rebuilds source in a publisher
job.

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

Tag-only detectors recognize artifact formats whose version authority is a
canonical Git tag. Each suggests a primary `{id}@{version}` tag:

| Detector             | Evidence                                                                           | Suggested identity                                    |
| -------------------- | ---------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `github-action`      | `action.yml` or `action.yaml`                                                      | containing directory name                             |
| `terraform-module`   | every non-ignored directory holding `.tf` files                                    | directory name                                        |
| `terraform-provider` | `go.mod` directly requiring `terraform-plugin-framework` or `terraform-plugin-sdk` | Go module path                                        |
| `docker-image`       | `Dockerfile`, `Dockerfile.*`, or `*.Dockerfile`                                    | filename variant, otherwise containing directory name |

`github-action`, `terraform-module`, and `docker-image` carry no version in
their own files, so they suggest no projection and no raw version.
`terraform-provider` is a Go module: it keeps the `go` projection at mode
`none`, which writes no version but lets a major bump rewrite the module path
suffix.

One Terraform module candidate covers one directory. The directory is the
candidate path, and its evidence digest covers every `.tf` file it holds, so
receipt identity never depends on which files are present: a managed module
keeps its receipt when a `.tf` file is added, renamed, or deleted, and an
excluded module reopens whenever any of its `.tf` files changes. A module at
the repository root is the candidate path `.`. Terraform providers are Go
modules, so their `go.mod` produces exactly one candidate under the more
specific provider detector, and only a direct `require` counts — `replace`,
`exclude`, and `// indirect` entries name a plugin module without depending on
it. Terraform example or test directories inside a provider repository still
surface as module candidates and are resolved as projections or exclusions.

The Docker/OCI detector reads the filename only. It never parses build
instructions, workflows, Compose files, registry coordinates, or image names,
and a candidate makes no claim that the image is built or published. Rolling
tags, `latest` aliases, and registry retention stay publisher policy. A
`Dockerfile.<variant>` suffix reads as a file extension, so companion documents
and copies such as `Dockerfile.md`, `Dockerfile.example`, and `Dockerfile.bak`
are not build definitions. A `.devcontainer/Dockerfile` builds a development
environment rather than a released image, and `.devcontainer` is hard-excluded
from the walk, so it produces no candidate.

A path that yields no usable id still produces a candidate with a tag
suggestion, plus an `identity-not-path-derivable` extraction diagnostic; name
the release unit explicitly in the resolution. That covers a `Dockerfile` or
`action.yml` at the repository root, and any segment Git would refuse inside a
tag — one beginning with `.` or `-`, containing `..`, or ending in `.lock`.

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

A tag-only candidate carries the same shape without version-bearing fields:

```yaml
discovery-candidates:
  - id: candidate:1cbb4b7e2f6c5f2d1a3f4b5c6d7e8f90112233445566778899aabbccddeeff00
    detector: terraform-module
    path: modules/network
    evidence:
      - path: modules/network
        digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      - path: modules/network/main.tf
        digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      - path: modules/network/variables.tf
        digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
    native-identity: network
    tag:
      id: primary
      role: primary
      template: "{id}@{version}"
    resolution:
      kind: independent
      release-unit: network
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
old exclusion. A directory-scoped candidate carries its directory first in the
evidence list, digested over the ordered path and digest of every member file,
and that entry is the digest a receipt binds. Paths are literal
workspace-relative paths, not globs. Init scans configured repositories too:
matching receipts make reruns no-ops, while new or changed excluded paths
reopen the same candidate workflow.

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
  --version runtime=1.4.0 \
  --version workspace/release=1.2.3
```

Every GitHub Action, Terraform module, Terraform provider, and Docker/OCI
release unit is tag-only, so each one needs its own `--version` until its
primary tag exists. Where a stream already has published tags, supply the
version that stream last released so the baseline continues it rather than
restarting it.

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
records before creating dependent tags. An exact existing selected record
satisfies the operation, so dry-run verification and retries after partial or
complete tag creation are idempotent. Any record mismatch still fails closed.
External continuous delivery owns all publication sequencing and registry
evidence.

### Self-hosted repository releases

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
task cargo-homebrew:compatibility
task ci
```

`task ci` is local preflight. It does not reproduce GitHub runner images,
architecture matrices, or pull-request state. Before starting work from an
integration branch, and after each change lands there, query its pull request:

```console
task hosted:check PR=<pull-request-number>
```

Every repository check must pass at the integration head. A red check is
work to diagnose or assign before the next change treats that head as its base.

`task cargo-homebrew:compatibility` needs Docker. It executes the derived
Cargo/Homebrew archive body from a real Cargo workspace member and executes the
derived macOS archive command with bsdtar 3.4.3. That older libarchive run proves
the command, archive content, executable mode, and timestamp contract. It does
not reproduce every property of the hosted macOS 14 runner.
