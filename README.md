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

The minimum supported Rust version is 1.88. The library crate is
`intentional-core`; the binary package is `intentional-cli` and installs the
`intentional` executable.

## Get started

Run `intentional init` in a Git repository. Resolve the generated candidates,
rerun initialization, and verify current-version authority. Then declare
changes with intents, apply a release plan, commit, and tag.

- [Initialization](https://wyrd.foo/docs/intentional/initialization): candidate
  resolution examples, already released repositories, Go baselines, and
  Changesets adoption.
- [Usage](https://wyrd.foo/docs/intentional/usage): intents, release plans, tags,
  build versions, and prerelease channels.
- [Configuration](https://wyrd.foo/docs/intentional/config): release units,
  projections, tag templates, dependency propagation, and release groups.
- [Installation](https://wyrd.foo/docs/intentional/install): platforms, binary
  verification, and GitHub Actions.
- [Overview](https://wyrd.foo/docs/intentional/overview): the release model and
  supported ecosystems.

Print the complete agent workflow guide embedded in your installed version:

```console
intentional skill
```

The output is a valid `SKILL.md` that can be installed in an agent's skill
directory.

## Development checks

```console
task build
task fmt-check
task lint
task test
task ci
```
