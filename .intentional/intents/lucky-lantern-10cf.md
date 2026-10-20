---
intentional: minor
---

Make `intentional verify` check a baseline tag: it recomputes the baseline set at the tagged commit from configuration and history, compares the baseline digest under the tag record's generator version, and checks that every tag `tag --baseline` creates for that set carries a baseline tag record for the same digest under the fixed tagger. A stream leaves the recomputed set only through a tag record on an earlier commit that has the fixed tagger and agrees with its tag; every tag record at the baseline commit is checked as a baseline tag. A tag without a tag record on an earlier commit supplies its version and is not checked. A differing digest fails with `baseline-digest-mismatch`, or is the advisory `producing-version-differs` for another generator version; the `baseline-record` diagnostic is removed. The report adds `baseline`, and a verified baseline reports the `baseline-reconstruction` guarantee.
