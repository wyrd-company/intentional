---
docs: true
title: Installation
order: 2
install: true
relationships:
  implements: intent-driven-polyglot-release
---

Intentional ships as a native executable named `intentional`. Homebrew and the
public npm launcher install matching GitHub Release binaries. Cargo builds the
native executable from the published Rust crates.

## Crate layout

The project is a Cargo workspace with two crates:

| Crate | Role |
| --- | --- |
| `intentional-core` | Library with the release and versioning logic. |
| `intentional-cli` | Binary package; installs the `intentional` executable. |

Install `intentional-cli`, not `intentional-core`, to get the command-line tool.

## Homebrew

```console
brew install wyrd-company/tools/intentional
```

## Cargo

```console
cargo install intentional-cli --locked
```

`--locked` builds against the crate's published lockfile. The minimum supported
Rust version is 1.82.

## npm and npx

Install the public scoped launcher globally:

```console
npm install --global @wyrd-company/intentional
intentional --version
```

Or run it without a global installation:

```console
npx --yes @wyrd-company/intentional --version
```

The npm package does not contain a second implementation. During installation
it selects the matching Intentional GitHub Release archive, downloads it, and
verifies its SHA-256 checksum against the release checksum evidence before
installing the native executable. The launcher then forwards arguments,
standard streams, signals where the platform supports them, and the native
process exit status.

The npm launcher supports these released platform and architecture pairs:

| Operating system | Architecture |
| --- | --- |
| Linux | x64, arm64 |
| macOS | arm64 |
| Windows | x64 |

Use Cargo or a source build on other platforms.

## GitHub Release binaries

Each GitHub Release contains the native archives listed above and a
`SHA256SUMS` file. Download both from the same bare-SemVer release, verify the
archive before extraction, and place the `intentional` executable on your
`PATH`. The archive names are:

- `intentional-linux-x86_64.tar.gz`
- `intentional-linux-arm64.tar.gz`
- `intentional-macos-arm64.tar.gz`
- `intentional-windows-x86_64.zip`

Use this path when a package manager is unavailable or when an installation
harness needs the native release artifact directly.

## GitHub Actions

The repository is also a composite GitHub Action for release automation. Pin
the `uses` reference and the installed binary to a released bare Semantic
Versioning tag:

```yaml
- uses: wyrd-company/intentional@0.1.1
  with:
    intentional-version: 0.1.1
    command: check
```

The Action currently supports Linux x64 and arm64 runners. It downloads the
matching native GitHub Release binary, places it on `PATH`, and runs one
Intentional command. The Action is plumbing for an existing release workflow;
it does not publish packages or own higher-level release choreography.

## From source

Clone the repository and build with Task or Cargo:

```console
task build
cargo run --bin intentional -- --help
```

To install the binary from a local checkout:

```console
cargo install --path crates/cli --locked
```

## Verify

```console
intentional --version
```

The command prints the installed version and confirms that `intentional` is on
your `PATH`.
