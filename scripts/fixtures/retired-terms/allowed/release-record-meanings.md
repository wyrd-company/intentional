The tag message starts with the header `intentional release record`, followed
by one blank line and the tag record fields.

```text
intentional release record

contract: contract-1
```

Every release of the unit has a release record on its primary tag, such as a
GitHub Release. The release-record policy is `required` or `none`; a unit with
`none` has no release record and its tag records are its complete record.
The process owns every push, build, release record, and publication.
Intentional records this file as evidence without interpreting its contents.
`verify` reports a baseline tag as `baseline-record`.
