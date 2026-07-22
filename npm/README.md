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
platforms. The installer opens direct HTTPS connections; environments that
require an explicit outbound proxy can use Cargo or download a release archive
through their managed transfer path.

Licensed under Apache-2.0.
