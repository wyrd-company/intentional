---
intentional: minor
---

A projection `file` that starts with `/` is relative to the repository root, so that several release units can write their versions into one shared `json`, `toml`, or `yaml` file while each changelog stays at its release-unit path. Configuration load refuses two projections that write the same field of one file, and `status`, `check`, `apply`, `stamp`, and `tag` also refuse them when they reach one file through a symlink, a hard link, or letter case. Every command refuses a projection file or changelog that resolves outside the repository.
