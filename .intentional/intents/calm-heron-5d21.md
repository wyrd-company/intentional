---
intentional: minor
---

Add the optional Release Unit field `release-record`, `required` (the default) or `none`, to `.intentional/config.yml` and its schema. It states whether the surrounding release process creates a release record, such as a GitHub Release, for each release of the unit; Intentional creates none and its commands behave the same for both values. Configuration without the field is unchanged. `intentional-core` adds `ReleaseUnitConfig::release_record` and the `ReleaseRecordPolicy` enum.
