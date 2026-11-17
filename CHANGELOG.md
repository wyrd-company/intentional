# Changelog

## 0.5.0

### Breaking

- Add `$schema` (`https://intentional.foo/schemas/0.3/release-plan`) and `source`, the commit and tree the plan was computed from, to the Release Plan that `plan` emits. Both are inside the plan digest. A plan without them keeps its recorded digest, and `tag --plan` still accepts it. `tag --plan` refuses a plan whose `source` is not the commit its release was planned from.
  
  `intentional-core` breaking change: `ReleasePlan` gains the public fields `schema` and `source`, so code that builds a `ReleasePlan` with a struct literal or matches it exhaustively must name them.
- `intentional-core` breaking change: the public record and result types are marked `#[non_exhaustive]`, so code outside the crate can no longer build them with a struct literal or match them exhaustively. This covers the Release Plan types (`ReleasePlan`, `PlanSource`, `PlanReleaseUnit`, `PlanTag`, `Generator`, `ChangelogEntry`), `TagRecord` and `TagPrerequisite`, the configuration, init, intent, tag, apply, stamp, status, verify, and version result types, and the `Error`, `SchemaRecord`, and `Adapter` enums. A later added member or variant is then a minor change. New: `TagPrerequisite::new` and `ChangelogEntry::new`.
- Return a ready Initialization Plan from ordinary init --json, with configuration under inferred-config instead of at the top level. Use the published initialization-plan schema identity. Include ordered release process next actions in ready initialization output: write an absent Release Model only when it remains absent after planned operations, then apply a release process template.
- `intentional-core` breaking change: `ReleasePlan::to_canonical_json` is replaced by `ReleasePlan::encode`, which refuses a plan that breaks the published schema, and `Error` gains the `Schema` variant, so exhaustive matches on `Error` must handle it. New: `ReleasePlan::decode`, the `TagRecord` codec, `baseline_digest`, and the `schema` module.
- Moving to this release. Configuration stays at `contract-2`, and the tag records and Release Plans this release writes carry `contract: contract-2`; records made under `contract-1` stay valid history. A repository still on a `contract-1` configuration (Intentional 0.3.x or earlier) runs `intentional init`, reviews any bump translation the init plan proposes, and commits the upgraded `.intentional/config.yml` with any rewritten intents; every other command refuses `contract-1`.
  
  A repository on 0.4.x runs `intentional model` and commits `.intentional/release-model.json` with root `.gitattributes`, so that a release process template can read the Release Model; from then on `check` fails and `status` reports `release-model-stale` when the model falls behind the configuration. Tools that read ordinary `init --json` output find the configuration under `inferred-config`. Tags that earlier releases created, including releases made under `contract-1` with `pre-1-0-bump-mapping: component`, verify with `intentional verify <tag>`, which rebuilds them under the contract they were made under.
  
  Intentional generates no CI workflow and runs none. A release process template, rendered by a template tool such as Toha, reads the Release Model and owns the workflow, its commits, pushes, publication and credentials.

### Features

- Accept a `v` version prefix in Release Unit projection tag templates where the
  ecosystem requires it, such as Go module tags. Primary and workspace tag
  templates still reject the prefix.
- Add `intentional verify <tag>`, which verifies one established Release from its annotated tag with Git history only: the tag record and tagger, the plan recomputed at the commit the release was planned from against the recorded digest, the files the release commit materialized, and the tag records of every identity tag. It exits 1 with a typed diagnostic on any failure and prints a structured report with `--json`. A digest difference for a tag produced by another Intentional version is an advisory. `intentional-core` adds `verify_release` and its report types.
- Add the optional Release Unit field `release-record`, `required` (the default) or `none`, to `.intentional/config.yml` and its schema. It states whether the surrounding release process creates a release record, such as a GitHub Release, for each release of the unit; Intentional creates none and its commands behave the same for both values. Configuration without the field is unchanged. `intentional-core` adds `ReleaseUnitConfig::release_record` and the `ReleaseRecordPolicy` enum.
- Add intentional model to write a canonical, schema-validated Release Model for release process templates. Initialization writes the model with configuration and resolves empty MSBuild PackageId through AssemblyName or the project file name.
- Report `release-model-stale` from check and status when an existing Release Model differs from the working-tree inputs; regenerate it with `intentional model`. Add `status --json` with `release_model_stale` and expose the same condition through `WorkspaceStatus` in intentional-core.
- Make `intentional verify` check a baseline tag: it recomputes the baseline set at the tagged commit from configuration and history, compares the baseline digest under the tag record's generator version, and checks that every tag `tag --baseline` creates for that set carries a baseline tag record for the same digest under the fixed tagger. A stream leaves the recomputed set only through a tag record with the fixed tagger that agrees with its tag and sits on an earlier commit or is a tag of a Release reconstructed at the baseline commit; a tag without a tag record supplies its version and is not checked. Each Release Unit whose primary tag the baseline created must have its version-bearing projections at that commit at its baseline version, or verification fails with `baseline-projection-mismatch`. A baseline created from uncommitted configuration or projection versions fails, because verification reads Git history only. A differing digest fails with `baseline-digest-mismatch`, or is the advisory `producing-version-differs` for another generator version; the `baseline-record` diagnostic is removed. The report adds `baseline`, and a verified baseline reports the `baseline-reconstruction` guarantee.
- Keep Release Models fresh across CRLF Git checkouts: `model` and initialization
  ensure `.intentional/release-model.json text eol=lf` in root `.gitattributes`,
  preserving unrelated rules. Commit the attributes file with the model. `model`
  reports `written` when either file needs repair; dry-run changes neither file.
  
  `intentional-core` adds the optional `release_model_stale_message` formatter,
  shared by check and human status so that both report the same repair text.
- `intentional-core` adds `native_identity`, a public function that resolves the native package identity of a manifest from its adapter, path, and text, by the rule initialization uses. It returns no identity, not an error, when a readable manifest declares none, and none for the generic JSON, TOML, and YAML adapters. It returns an error only when the manifest cannot be parsed for its adapter.

### Fixes

- Call the annotated tag message a tag record in `tag` help and in diagnostics,
  so that it is not confused with the GitHub Release published for a release unit.
- Correct agent initialization guidance to commit configuration, the Release
  Model, and root .gitattributes before applying a release process template, and
  count both model and attributes writes in a ready imported plan.
- Write the compatibility-line schema identity `https://intentional.foo/schemas/0.2/config` as `$schema` when `init` creates configuration. Configuration that carries `https://intentional.foo/schemas/config.yml` still loads.
- Validate a Release Plan read by `tag --plan` and every tag record that `tag`, `status`, and `check` read against their published schemas. A violation names the record, the failing location, and the schema keyword, and the command exits 1. A tag record must also begin with the protocol header and list its fields in canonical order. Plans and tag records that released versions wrote still pass.

## 0.4.1

### Fixes

- `init` no longer fails on a `package.json` without a string `name`, such as a private manifest that only holds dependencies. Like a Cargo manifest without `[package]`, it describes no versioned project, so it is not a discovery candidate. A repository with such a manifest can now run `init`, including the `contract-1` upgrade.

## 0.4.0

### Breaking

- Configuration contract `contract-2` replaces `contract-1`. A bump name always states the significance of a change: before 1.0.0, `major` gives `0.(x+1).0`, and `minor` and `patch` give `0.x.(y+1)`. `settings.pre-1-0-bump-mapping` is removed. Every command except `init` refuses a `contract-1` configuration and says to run `intentional init`, which upgrades it, keeps its comments, and accepts release tags created under `contract-1`. A `component` configuration with pending intents gets a reviewable `bump-translation` choice in the init plan, or `init --bump-translation parity|significance`.
  
  An intent can graduate a `0.x` release unit to exactly 1.0.0 with `{ bump: major, graduate: true }`, or `add --graduate <id>`. Graduation is the only way to reach 1.0.0. It graduates a whole fixed group, or the releasing members of a linked group, and `check` fails when the release unit is already at 1.0.0 or later. `status`, the release plan (`graduates`), and the changelog report the graduation.
  
  Changesets adoption translates pending changesets once. The default `bump-translation: parity` keeps the versions that Changesets would release, so `major` at `0.x` becomes `major` with `graduate`, and `minor` becomes `major`; each renamed bump gets a note in the intent. `significance` keeps the bump names, and the parity comparison reports the version changes as `expected-difference`.

## 0.3.0

### Breaking

- `check` fails when a `committed` projection holds a version other than the tag-derived current version, naming the release unit, file, pointer, found version, and expected version. When a release unit has an applied release that waits for its tag, every `committed` projection of the unit must hold that release version, including a prerelease above the last stable tag, so the commit of a full apply passes and a partly applied release fails. An existing changelog whose leading heading is not plain SemVer supplies no applied release to `check`, which compares projections with the tag-derived version. `tag` retains strict release validation. `status` reports drift on `injected` projections without failing `check`; neither command reads `none` projections.

## 0.2.1

### Features

- A projection `file` that starts with `/` is relative to the repository root, so that several release units can write their versions into one shared `json`, `toml`, or `yaml` file while each changelog stays at its release-unit path. Configuration load refuses two projections that write the same field of one file, and `status`, `check`, `apply`, `stamp`, and `tag` also refuse them when they reach one file through a symlink, a hard link, or letter case. Every command refuses a projection file or changelog that resolves outside the repository.

### Fixes

- Refuse a baseline when an existing release record's tag id, version, contract, or baseline field does not match the tag it occupies.
- `apply` and `stamp` no longer lose an edit when release units reach one shared projection file through different paths, such as symlinks, letter case, or a `./` prefix. A `json` pointer now selects an array element only by its index without leading zeros, so that `/versions/01` cannot write the same element as `/versions/1`.
- Document that an internal dependency bump propagates only between release units that share an ecosystem adapter.

## 0.2.0

### Breaking

- Building from source now requires Rust 1.88 or later, so that Intentional can use gix 0.89.

### Fixes

- Clarify repository initialization and existing-release bootstrap in the documentation and embedded agent skill; keep the README focused on getting started.

## 0.1.8

### Fixes

- Establish a baseline for a release unit added after other release units have released.

## 0.1.7

### Fixes

- Keep every release unit's edit when units at different paths project into one shared file.
- Tolerate annotated pre-Intentional tags as history in tag records.
- Tag releases in which only some release units moved; units already at their released version are no longer treated as part of the next release.
- Generate Homebrew formulae that pass strict audit and style checks.

## 0.1.6

### Fixes

- Add deterministic tagger headers to annotated release records for strict Git integrity checks.
- Accept sealed release plans generated by the immediately preceding published Intentional version during self-hosted tagging.
- Published Linux GNU archives support Ubuntu 22.04 LTS through the pinned cross-rs compatibility baseline.

## 0.1.5

### Fixes

- Correct initialization plans, tag retries, and native archive integrity verification.

## 0.1.4

### Fixes

- Make initialization repository-complete and safely remove authorized release-tool proxies.

## 0.1.3

### Features

- Embed the agent workflow skill in the CLI.

## 0.1.2

### Features

- Detect Dev Container Features and Templates as explicit release-unit candidates.

### Fixes

- Generate portable release checksums that validate downloaded assets directly.

## 0.1.1

### Fixes

- Include the project README in both published Rust crates.

## 0.1.0

Initial release.

### Features

- Intent-driven polyglot versioning across npm, Cargo, Pub, Python, MSBuild,
  and Go, with format-preserving JSON, TOML, and YAML projections.
- The `init`, `add`, `status`, `plan`, `apply`, `stamp`, `tag`, and `check`
  commands, canonical digest-bound release plans, changelogs, drift detection,
  channel releases, and dry runs.
- A tag-triggered release pipeline and a plumbing GitHub Action.
