---
docs: true
title: Usage
order: 3
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

## Bootstrap the workspace

`init` requires a Git repository. It recursively discovers supported manifests
within the repository's Git-ignore and hard-cache boundary. Package-manager
workspace membership informs recommendations, dependency analysis, and
Changesets parity but never limits discovery.

```bash
intentional init
```

Every discovery result first appears as a candidate in
`.intentional/init-plan.yml`. Resolve each candidate as:

- `independent`: create a release unit;
- `projection`: attach the manifest to another release unit; or
- `excluded`: record why the manifest is outside Intentional's inventory.

Resolution fields use the schema's kebab-case spelling:

```yaml
resolution:
  kind: projection
  release-unit: sample-library
  target-candidate: candidate:0000000000000000000000000000000000000000000000000000000000000000
```

Rerun `intentional init` after setting the explicit resolutions. Intentional
writes `.intentional/config.yml` only after the complete candidate graph
validates. This prevents discovery guesses from silently becoming release
authority.

Candidates retain detector/path identity even when multiple manifests declare
the same native name. Explicit resolutions determine ownership before final
configuration validation.

During Changesets adoption, a manifest mapped onto another artifact can expose
an explicit `retain` or `remove` integration choice. Probable-proxy diagnostics
separate supporting and contradictory structural evidence and state their
uncertainty. Prose and semantic keyword matching are not evidence. A removal is
performed only during explicit takeover, after the resolution authorizes it and
a fresh repository-use check finds no non-Changesets references.

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
versions, manifest drift, tag-record issues, and missing baselines.
`check` validates configuration, intents, tag records, baselines, and
deterministic planning for continuous integration.

## Preview the plan

`plan` writes canonical, digest-bound release-plan JSON to standard output
without changing anything. It includes changed release units, contributing
intents, release notes, expected tags, required phases, and tag order:

```bash
intentional plan > release-plan.json
```

## Project the versions

`apply` writes release versions into committed projections, rewrites internal
dependency ranges, updates each release unit's changelog, and consumes the
included intents. It edits only the working tree:

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

If a tag requires an executor phase, declare it explicitly:

```bash
intentional tag --plan release-plan.json --phase before-publication
```

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
intentional tag --channel beta
```

A channel `apply` retains intents; a later channel-less `apply` consolidates the
prerelease changelog sections into the final release.

## Adopt a Changesets repository

When `.changeset/config.json` exists, ordinary `init` preserves Changesets as
the authority and writes an adoption plan. Resolve candidate ownership,
ignored-package disposition, contract differences, and the reported repository
integrations until the plan proves release parity. Then preview and perform the
explicit takeover:

```console
intentional init
intentional init --take-over --dry-run
intentional init --take-over
git add -A
git commit -m "Adopt Intentional"
intentional tag --baseline
```

Takeover changes only Intentional and recognized Changesets state in one
rollback-capable transaction. Repository-specific scripts and workflows remain
the user's responsibility. Baseline tags are created against the externally
committed takeover state, so the authority boundary stays explicit.

## Configure the GitHub executor

Opt a repository into the GitHub executor and its explicit publication intent:

```console
intentional executor init
```

The first run writes `.intentional/executor-init-plan.yml` and exits with code
`2`. It proposes every publishable package inside each release unit, including
workspace manifests, image definitions, and Go commands. Set each candidate
`resolution` to one declared destination choice or `decline`, then rerun the
command until it reports the `ready` state and updates `.intentional/config.yml`.
An acceptance writes the package and chosen destination. Declining a package
writes an evidence-pinned discovery receipt. Declining a destination writes its
four-segment identity under `github.declined-publications`, which suppresses the
same offer without changing configured publications. These decisions survive a
fresh clone even though the plan does not. A later manifest change invalidates
a package decline receipt and reopens that decision against new evidence.

Add `--dry-run` to print the plan the command would write, and every file it
would touch, without changing the workspace.

Initialization reports the repository settings Intentional never mutates: the
GitHub App must be a ruleset bypass actor for the default branch and every
managed release tag namespace, the App credentials must exist as repository
secrets, and the protected release environment must guard the authority
transition.

Validate the result whenever configuration or native packager files change:

```console
intentional executor check
```

The check resolves every configured publication to exactly one maintained
recipe, verifies the native packager configuration that recipe requires, and
reports missing workflows, gate jobs, and managed workflow drift. It uses the
same comparison engine as the diff below, so what the check reports is exactly
what a diff would change.

A Rust command can publish through Homebrew, RPM, APT, or the Arch User
Repository (AUR). The package must expose exactly one `[[bin]].name`, or a
package binary at `src/main.rs`. Intentional builds Linux x86-64 and Linux Arm64
through digest-pinned Cross 0.2.5 images and builds macOS Arm64 with Cargo. The
aggregate job creates the configured distribution outputs from those archives
and seals them together. Publisher jobs do not invoke Cargo.

For Homebrew, declare `homebrew.repository` as the tap's `owner/name`. The
binary's `--version` output must contain the release version for the generated
formula's installation test. The tap must install the release GitHub App so the
publisher job can mint a token scoped to that repository.

For RPM or APT, provide the repository-owned composite delivery Action, public
repository base URL, public index-signing-key URL, observation deadline, and
index coordinates. These routes currently package only the Linux x86-64
archive. The Cargo package must declare at least one author, which becomes the
downstream package maintainer. RPM requires a channel. APT requires a suite and
component.
The optional `with` mapping supplies the delivery Action's remaining inputs.
The Action receives the sealed package path, format, name, version,
architecture, digest, and coordinates under the configured executor prefix.

For AUR, declare `aur: {}` and provide `INTENTIONAL_AUR_KEY` as a repository
secret. Intentional derives `<binary>-bin` as the package repository, generates
`PKGBUILD` and `.SRCINFO` from the two Linux archive digests, pins the AUR host
key, and pushes only those sealed descriptors. A new package is created by its
initial push. The generated `-bin` package provides and conflicts with
`<binary>`, the non-empty base identity obtained by removing the final `-bin`
suffix. Descriptor generation fails when the destination does not have that
form.

## Reconcile the managed workflow slices

Intentional owns complete authority-bearing slices of your release and publish
workflows without owning the documents. Compare a workflow with the contract
derived from your configuration:

```console
intentional executor diff release
```

The comparison is read-only. It prints a unified patch bound to a digest of the
exact workflow bytes it read, and `--format json` prints the same result as a
structured document. Apply it when you are satisfied with what it proposes:

```console
intentional executor diff release --apply
```

Apply re-reads the workflow and refuses the transformation if the file changed
after the patch was computed, so a stale patch never overwrites newer content.
Use `--workflow PATH` to compare a candidate file instead of the configured one.

The comparison reads workflows of at most 2000 lines, for configured and
`--workflow` inputs alike, and refuses anything larger with a
`workflow-too-large` diagnostic. The bound admits the complete managed release
graph while refusing inputs large enough to make quadratic comparison costly.

A comparison can also report advisories you should read before applying it. The
safe top-level permission default withdraws workflow-level scopes such as
`id-token: write`; the comparison names each one it drops so a repository-owned
job that needs it can declare it per job.

Reconciliation adds the required triggers, the release concurrency policy, a
safe top-level permission default, and the complete managed jobs, and wires each
configured gate into the job it governs. Your own triggers, jobs, comments, and
formatting are preserved. Every managed job carries the reserved step id
`intentional_executor_contract`, which is how Intentional recognizes its own
slices after you change the configured prefix: sentinel-bearing jobs under the
old prefix are replaced, and a job that merely happens to share the old prefix
stays yours.

## Read the authority split in the maintained slice

The release workflow derives two managed jobs, and the boundary between them is
the point where a release gains the authority to write to your repository.
Preparation holds none of it:

```yaml
  intentional_prepare:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - id: intentional_executor_contract
        name: Check out the accepted source commit
        uses: actions/checkout@<pinned>
        with:
          fetch-depth: 0
          fetch-tags: true
          persist-credentials: false
```

It builds the release candidate and uploads it as an artifact. It runs in no
environment, mints no token, and checks out without persisting credentials, so
nothing it does can reach the repository.

The second job is where authority appears, and it appears only after the
candidate has been verified:

```yaml
  intentional_release:
    needs:
      - intentional_prepare
    environment: intentional-release
    permissions:
      contents: read
    steps:
      - name: Check out the repository without persisted credentials
      - name: Download the release candidate
      - name: Verify the release candidate handoff
      - name: Mint a short-lived repository token
      - name: Publish the release commit and the global release tag
      - name: Create the draft GitHub Release for the published tag
```

The order is the design. Verification precedes the token, so a candidate that
fails verification never reaches a step that can write. The `contents: read`
permission is the workflow token's, not the App token's: the job writes with the
short-lived token it mints, which is why the declared permission stays read-only
even in the job that publishes.

Every managed checkout requests `fetch-depth: 0` and `fetch-tags: true`.
Verification rebuilds the candidate from the accepted source commit and derives
versions from annotated tags, so a shallow or tagless checkout fails
verification rather than producing a wrong release. Publisher and retrieval
jobs keep the same requirement even though they promote an already-built
subject: after delivery, `intentional verify publication` loads the released
configuration, reconstructs the planned release from the annotated global tag,
and reads phase tags before it accepts the publication observation. A shallow
publisher checkout would therefore remove evidence the verification step uses.

## Prepare the repository

Intentional never mutates repository settings. `intentional executor init`
reports what you must configure, and `intentional executor check` reports it
again for as long as it is missing.

**Ruleset bypass.** The release GitHub App must be a bypass actor for the
default branch and for every managed release tag namespace. The release job
pushes the release commit and the annotated global tag in one atomic update; a
ruleset that blocks either makes the authority transition fail after the
candidate has already been verified.

**App credentials, and they are not both secrets.** The repository must define
`INTENTIONAL_GITHUB_APP_ID` as a repository **variable** and
`INTENTIONAL_GITHUB_APP_PRIVATE_KEY` as a repository **secret**. The derived
jobs read them as `vars.INTENTIONAL_GITHUB_APP_ID` and
`secrets.INTENTIONAL_GITHUB_APP_PRIVATE_KEY`. Creating the App ID as a secret
instead leaves `vars.INTENTIONAL_GITHUB_APP_ID` empty and fails token minting in
the privileged job, after the candidate has already been verified. Together they
are the only long-lived repository-write credentials involved.

**Protected environment.** The `intentional-release` environment must exist and
must guard every irreversible authority transition. This includes jobs that
write to the repository or Release and every job that publishes to an external
destination. Requiring a reviewer, a wait timer or a branch restriction there
gates the step that spends each credential.

Store destination credentials as environment secrets and variables. For an
existing repository, copy each destination credential under the same name from
repository settings to `intentional-release`. Apply the regenerated workflow.
Prove one protected publication. Then remove the repository-level copy. The
repository-level value remains a working fallback during the migration, but any
workflow in the repository can still read it until it is removed. Intentional's
own repository migration set is `CARGO_REGISTRY_TOKEN` and `NPM_TOKEN`; its
Homebrew publisher continues to mint a tap-scoped token from the release App
credentials.

Verify it against your own derived slice rather than against this paragraph:

```console
intentional executor diff publish --apply
grep -n 'environment:' .github/workflows/publish.yml
```

**Names change with a configured prefix.** The environment, the App ID variable,
the App private key secret and the AUR key secret all derive from the `envvar`
and `job` prefixes, defaulting to `INTENTIONAL_`. Creating the default names
under a configured prefix fails the release after verification has already
passed. `intentional executor init` reports the names your configuration
derives — create those rather than the ones written here.

**Publisher credentials** vary by destination, and Intentional stores names,
never values. Put stored destination values in the protected environment. Some
destinations authenticate with the job's own token or with one the job mints
from the release App. The next section has the full table. A tap repository
must install the release App so the publisher job can mint a token scoped to
that repository alone.

## Publish to a registry for the first time

Configuration has two levels. A **publisher** is what you declare on a package.
A **destination** is where one publication lands. Intentional supports 9
destinations in total. npm, Cargo, and OCI destinations are explicit. An empty
`npm` or `cargo` mapping names no destination. `executor init` and `executor
check` report it, and release execution refuses it until the maintainer names a
target or removes the publisher mapping.
For example:

```yaml
npm:
  npmjs: {}
  github: {}
cargo:
  registry: {}
```

Each destination authenticates its own way.

| Publisher | Destination | Credential |
| --- | --- | --- |
| `npm:` | npmjs <!-- intentional-target: primary --> | `NPM_TOKEN`, then trusted publishing |
| `npm:` | GitHub Package Registry <!-- intentional-target: github --> | the job's `GITHUB_TOKEN` |
| `cargo:` | crates.io, or the one alternate registry `Cargo.toml` names <!-- intentional-target: primary --> | `CARGO_REGISTRY_TOKEN` |
| `homebrew:` | your tap <!-- intentional-target: primary --> | minted from the App private key |
| `rpm:` | your repository <!-- intentional-target: primary --> | inputs you pass your delivery Action |
| `apt:` | your repository <!-- intentional-target: primary --> | inputs you pass your delivery Action |
| `aur:` | the AUR <!-- intentional-target: primary --> | `INTENTIONAL_AUR_KEY`, an SSH private key |
| `oci:` | Docker Hub <!-- intentional-target: dockerhub --> | `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` |
| `oci:` | GHCR <!-- intentional-target: ghcr --> | the job's `GITHUB_TOKEN` and actor |

### npmjs and crates.io start with a token and stop using it

Both publish through registry trusted publishing, which needs no stored
credential — and neither registry will let you configure a trusted identity
until the package exists. So the first publication uses the token above, and
after you configure the trusted identity that token is unreachable on this path.

**The bootstrap path opens only on proof that the destination does not hold the
package.** A registry client reports a missing package and a failed request the
same way, and treating them alike would turn a transient registry outage into a
steady-state publication authenticated by a long-lived credential. Both recipes
separate the outcomes and fail the job on an inconclusive probe. A publication
failing this way is reporting that the probe did not complete, not that the
package is absent.

**Once the package exists, publication requires the trusted identity and never
falls back.** An identity failure fails the publication rather than reaching for
the token, so a misconfigured trusted publisher cannot silently revert to a
long-lived credential.

A Cargo registry other than crates.io has no trusted-publishing exchange to
bootstrap into, so it keeps using `CARGO_REGISTRY_TOKEN` on every publication.
It still probes the destination before submitting — that probe is not the
bootstrap probe, and it refuses to submit when the registry does not answer.

### Standing credentials

Docker Hub, the AUR, and a non-crates.io Cargo registry authenticate every
publication with the credential you stored. Rotate them on whatever schedule you
use for any standing publish credential. The AUR's is an SSH private key rather
than a scoped registry token, which is worth weighing separately.

### Credentials you do not store

GitHub Package Registry and GHCR use the job's own token, which expires when the
job ends. Homebrew mints a tap-scoped token from the release App, which is why
the tap repository must install that App.

### Credentials you name yourself

Intentional derives no credential for RPM or APT. Each routes through a
repository-owned delivery Action you name with `delivery-action`, and passes it
whatever you supply under that publisher's `with:` block. You choose the key as
well as the value, there is no conventional default to fall back to, and what
that Action authenticates with is yours to decide.

### Renaming a credential

The `npm.npmjs:` and `cargo.registry:` targets accept `token-secret`. The
`dockerhub` target **inside** `oci:` accepts `token-secret` and `username-var`.

The nesting matters. `oci: { token-secret: … }` is refused with *unknown field
`token-secret`, expected `dockerhub` or `ghcr`* — the override belongs to the
target, not the publisher holding it. `aur: { token-secret: … }` is also refused
with *unknown field `token-secret`, there are no fields*.

The AUR is the destination most likely to send you looking for this override,
but it does not have one. Its secret name is not fixed — it follows the
configured prefix — but it is derived rather than chosen, so the way to change
what it holds is to change the secret, not its name.
