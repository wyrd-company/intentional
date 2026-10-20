---
intentional: minor
---

`intentional-core` adds `native_identity`, a public function that resolves the native package identity of a manifest from its adapter, path, and text, by the rule initialization uses. It returns no identity, not an error, when a readable manifest declares none, and none for the generic JSON, TOML, and YAML adapters. It returns an error only when the manifest cannot be parsed for its adapter.
