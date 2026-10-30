---
intentional: patch
---

`init` no longer fails on a `package.json` without a string `name`, such as a private manifest that only holds dependencies. Like a Cargo manifest without `[package]`, it describes no versioned project, so it is not a discovery candidate. A repository with such a manifest can now run `init`, including the `contract-1` upgrade.
