---
relationships:
  implements: intent-driven-polyglot-release
---

# @wyrd-company/intentional

This package installs the native `intentional` executable from the matching
[`wyrd-company/intentional`](https://github.com/wyrd-company/intentional)
GitHub Release.

```console
npm install --global @wyrd-company/intentional
intentional --version
```

Run it without a global installation:

```console
npx --yes @wyrd-company/intentional --version
```

The installer downloads `SHA256SUMS` and the matching archive over HTTPS. It
requires an exact checksum entry before extracting only the expected native
executable. Redirect overflow, HTTP errors, missing checksums, checksum
mismatches, unsafe archive paths, and unsupported platforms stop installation.

Supported platforms are Linux x64 and arm64, macOS arm64, and Windows x64.
Install the `intentional-cli` crate with Cargo or build from source on other
platforms.

Licensed under Apache-2.0.
