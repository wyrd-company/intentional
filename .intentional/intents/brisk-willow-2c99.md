---
intentional: patch
---

Write the compatibility-line schema identity `https://intentional.foo/schemas/0.2/config` as `$schema` when `init` creates configuration. Configuration that carries `https://intentional.foo/schemas/config.yml` still loads.
