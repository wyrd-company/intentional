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
`resolution` to one declared publisher choice or `decline`, then rerun the
command until it reports the `ready` state and updates `.intentional/config.yml`.
An acceptance writes the package and its publisher opt-in. A decline writes an
evidence-pinned discovery receipt. Both decisions therefore survive a fresh
clone even though the plan does not. A later manifest change invalidates a
decline receipt and reopens the package decision against the new evidence.
Accepting a package can offer its dependent targets on the next run, so the plan
converges through explicit decisions rather than inference.

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
`workflow-too-large` diagnostic. A GitHub workflow is far smaller than that; a
file that reaches this size is almost certainly not a workflow.

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
