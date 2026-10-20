---
intentional: patch
---

Validate a Release Plan read by `tag --plan` and every tag record that `tag`, `status`, and `check` read against their published schemas. A violation names the record, the failing location, and the schema keyword, and the command exits 1. A tag record must also begin with the protocol header and list its fields in canonical order. Plans and tag records that released versions wrote still pass.
