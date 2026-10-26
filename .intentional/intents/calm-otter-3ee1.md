---
intentional: patch
---

`apply` and `stamp` no longer lose an edit when release units reach one shared projection file through different paths, such as symlinks, letter case, or a `./` prefix. A `json` pointer now selects an array element only by its index without leading zeros, so that `/versions/01` cannot write the same element as `/versions/1`.
