---
intentional: patch
---

`apply` and `stamp` no longer lose an edit when release units reach one shared projection file through different paths, such as symlinks, letter case, or a `./` prefix.
