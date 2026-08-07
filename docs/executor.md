---
docs: true
title: GitHub executor
order: 4
relationships:
  implements: github-release-executor
---

The GitHub release executor is optional. Intentional versioning, projection,
planning, and annotated release records work without it. Add the executor when
you want Intentional to generate repository and publication choreography from
the same configuration.

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

Intentional generates complete authority-bearing slices of your release and
publish workflows. You own the workflow documents. Compare a workflow with the
contract derived from your configuration:

Read the [derived publish workflow](publish-workflow.md) before applying the
publish slice. It explains the job kinds, job separation, and credential
boundaries.

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

A comparison can also report advisories you should read before applying it. The
safe top-level permission default withdraws workflow-level scopes such as
`id-token: write`; the comparison names each one it drops so a repository-owned
job that needs it can declare it per job.

Reconciliation adds the required triggers, the release concurrency policy, a
safe top-level permission default, and the complete generated jobs, and wires
each configured gate into the job it governs. Repository triggers, jobs,
comments, and formatting outside the generated slice are preserved. Every
generated job carries the reserved step id `intentional_executor_contract`,
which is how Intentional recognizes generated slices after you change the
configured prefix: sentinel-bearing jobs under the old prefix are replaced,
and a job that merely happens to share the old prefix stays yours.

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
Verification, build, tag, assembly, and closure jobs read release identity or
version authority from repository tags. Upload, publisher, and retrieval jobs
use the same checkout contract. Moving a portable command between managed job
roles therefore preserves its view of the released commit and tags.

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

Store destination credentials as environment secrets and variables. To migrate
an existing repository-level value, copy it under the same name to
`intentional-release`, apply the derived workflow, and prove one protected
publication. Remove the repository-level value after that proof. Any workflow
in the repository can read a repository-level secret until it is removed.

The environment also becomes part of each publisher's OpenID Connect claim set
and default subject. Before applying the derived workflow, update the
npmjs and crates.io trusted-publisher records to name `intentional-release`, or
the identity exchange will no longer match. Under a configured prefix, name the
environment that prefix derives instead. This registry-record migration covers
the identity routes that store no repository secret.

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
No maintained read-only identity can be minted for an arbitrary Cargo
registry, so authenticated observation stays in the protected publisher job
and reuses its existing credential. GitHub Package Registry differs: its
downstream observer uses a job token limited to `packages: read`. In both cases,
only the completed observation reaches Intentional's Action; no registry token
is passed to it.

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
