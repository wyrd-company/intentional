// ---
// relationships:
//   tests: tag-record
// ---

//! The Tag Record codec against the published fixtures.

use intentional_core::schema::SchemaRecord;
use intentional_core::{
    baseline_digest, canonical_json, sha256_digest, Error, TagPrerequisite, TagRecord,
    TagRecordRole,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const RELEASED_0_2_0_RECORDS: [&str; 3] = [
    "released-0.2.0/release-unit-tag-record.txt",
    "released-0.2.0/workspace-tag-record.txt",
    "released-0.2.0/baseline-tag-record.txt",
];
const COMPLETE_RECORD: &str = "tag-record/complete.txt";
const RELEASED_BASELINE_DIGEST: &str =
    "sha256:f4cd063069d178dbd03fce40c1f38c190a29aab6359aa0eb0991ec3f54451f07";

fn fixture(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/specifications/fixtures")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn canonical_tag_records_round_trip_to_their_bytes() {
    for name in RELEASED_0_2_0_RECORDS.into_iter().chain([COMPLETE_RECORD]) {
        let message = fixture(name);
        let record = TagRecord::decode(&message).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(record.encode().expect("encode"), message, "{name}");
    }
}

#[test]
fn tag_records_written_by_released_0_2_0_decode() {
    let record =
        TagRecord::decode(&fixture(RELEASED_0_2_0_RECORDS[0])).expect("released tag record");
    assert_eq!(
        record,
        TagRecord::new(
            "contract-1",
            "0.2.0",
            "sha256:cd4f1ebfe3db2b887ecfd646383c5887e48b261a4e7512efb02289ee867120e2",
            "release-unit/sample-library/primary",
            "1.3.0",
            false,
        )
    );
    let baseline =
        TagRecord::decode(&fixture(RELEASED_0_2_0_RECORDS[2])).expect("released baseline");
    assert!(baseline.baseline);
    assert_eq!(baseline.plan_digest, RELEASED_BASELINE_DIGEST);
}

#[test]
fn repeated_prerequisite_lines_project_into_the_prerequisites_array() {
    let record = TagRecord::decode(&fixture(COMPLETE_RECORD)).expect("complete record");
    assert_eq!(record.tag_role, Some(TagRecordRole::Workspace));
    assert_eq!(record.target.as_deref(), Some(&*"3".repeat(40)));
    assert_eq!(
        record.prerequisites,
        [
            TagPrerequisite {
                tag_id: "release-unit/sample-library/mirror".to_owned(),
                tag_object: "4".repeat(40),
            },
            TagPrerequisite {
                tag_id: "release-unit/sample-library/primary".to_owned(),
                tag_object: "5".repeat(40),
            },
        ]
    );
}

#[test]
fn encoding_orders_prerequisites_by_tag_id() {
    let mut record = TagRecord::decode(&fixture(COMPLETE_RECORD)).expect("complete record");
    record.prerequisites.reverse();
    assert_eq!(record.encode().expect("encode"), fixture(COMPLETE_RECORD));
}

#[test]
fn the_final_newline_is_optional() {
    let message = fixture(COMPLETE_RECORD);
    let unterminated = message.strip_suffix('\n').expect("final newline");
    assert_eq!(
        TagRecord::decode(unterminated).expect("record without final newline"),
        TagRecord::decode(&message).expect("record")
    );
}

#[test]
fn invalid_tag_records_fail_at_the_codec_for_their_stated_reason() {
    for (name, expected_location, expected_keyword) in [
        ("undeclared-member", "", "additionalProperties"),
        ("schema-member", "", "additionalProperties"),
        ("wrong-contract", "/contract", "const"),
        ("digest-pattern", "/plan-digest", "pattern"),
        ("semver-pattern", "/version", "pattern"),
        ("git-object-pattern", "/target", "pattern"),
        (
            "prerequisite-object-pattern",
            "/prerequisites/0/tag-object",
            "pattern",
        ),
        ("workspace-role-primary", "/tag-role", "const"),
        ("missing-required", "", "required"),
    ] {
        let message = fixture(&format!("tag-record/invalid/{name}.txt"));
        match TagRecord::decode(&message) {
            Err(Error::Schema {
                record,
                location,
                keyword,
                ..
            }) => {
                assert_eq!(record, SchemaRecord::TagRecord, "{name}");
                assert_eq!(location, expected_location, "{name}");
                assert_eq!(keyword, expected_keyword, "{name}");
            }
            other => panic!("{name}: expected a schema failure, got {other:?}"),
        }
    }
}

#[test]
fn malformed_messages_fail_at_the_codec() {
    let complete = fixture(COMPLETE_RECORD);
    let swapped = complete.replacen(
        "tag-id: workspace/release\ntag-role: workspace\n",
        "tag-role: workspace\ntag-id: workspace/release\n",
        1,
    );
    let prerequisites_reversed = complete
        .replacen("mirror 4444", "primary 4444", 1)
        .replacen("primary 5555", "mirror 5555", 1);
    let repeated_prerequisite = complete.replacen(
        "release-unit/sample-library/primary 5555",
        "release-unit/sample-library/mirror 5555",
        1,
    );
    for (name, message, reason) in [
        (
            "without header",
            complete.replacen("intentional release record\n\n", "", 1),
            "does not begin with the protocol header",
        ),
        (
            "fields out of schema order",
            swapped,
            "tag record is not canonical",
        ),
        (
            "prerequisites out of tag ID order",
            prerequisites_reversed,
            "tag record is not canonical",
        ),
        (
            "repeated scalar field",
            complete.replacen("baseline: false\n", "baseline: false\nbaseline: false\n", 1),
            "tag record repeats field baseline",
        ),
        (
            "repeated prerequisite",
            repeated_prerequisite,
            "names prerequisite release-unit/sample-library/mirror more than once",
        ),
        (
            "line without a field separator",
            format!("{complete}\n"),
            "is not a `key: value` field",
        ),
    ] {
        assert_ne!(message, complete, "{name}");
        match TagRecord::decode(&message) {
            Err(Error::Validation(detail)) => {
                assert!(detail.contains(reason), "{name}: {detail}");
            }
            other => panic!("{name}: expected a validation failure, got {other:?}"),
        }
    }
}

#[test]
fn baseline_digest_payload_has_its_recorded_digest_in_any_key_order() {
    let versions = BTreeMap::from([
        ("sample-library".to_owned(), "1.2.3".to_owned()),
        ("workspace/release".to_owned(), "0.1.0".to_owned()),
    ]);
    assert_eq!(
        baseline_digest("contract-1", "0.2.0", &versions).expect("baseline digest"),
        RELEASED_BASELINE_DIGEST
    );
    for name in [
        "released-0.2.0/baseline-digest-payload.json",
        "tag-record/baseline-digest-payload-reordered.json",
    ] {
        let payload: Value = serde_json::from_str(&fixture(name)).expect("payload JSON");
        assert_eq!(
            sha256_digest(canonical_json(&payload).expect("canonical").as_bytes()),
            RELEASED_BASELINE_DIGEST,
            "{name}"
        );
    }
    assert_eq!(
        canonical_json(
            &serde_json::from_str::<Value>(&fixture(
                "tag-record/baseline-digest-payload-reordered.json"
            ))
            .expect("payload JSON")
        )
        .expect("canonical"),
        fixture("released-0.2.0/baseline-digest-payload.json")
    );
}

#[test]
fn baseline_digest_refuses_a_payload_its_schema_rejects() {
    match baseline_digest("contract-1", "0.2.0", &BTreeMap::new()) {
        Err(Error::Schema {
            record,
            location,
            keyword,
            ..
        }) => {
            assert_eq!(record, SchemaRecord::BaselineDigestPayload);
            assert_eq!(location, "/versions");
            assert_eq!(keyword, "minProperties");
        }
        other => panic!("expected a schema failure, got {other:?}"),
    }
}

#[test]
fn encoding_refuses_a_record_its_schema_rejects() {
    let mut record = TagRecord::decode(&fixture(COMPLETE_RECORD)).expect("complete record");
    record.tag_role = Some(TagRecordRole::Primary);
    match record.encode() {
        Err(Error::Schema {
            record, keyword, ..
        }) => {
            assert_eq!(record, SchemaRecord::TagRecord);
            assert_eq!(keyword, "const");
        }
        other => panic!("expected a schema failure, got {other:?}"),
    }
}

#[test]
fn encoding_refuses_a_prerequisite_tag_id_named_twice() {
    let mut record = TagRecord::decode(&fixture(COMPLETE_RECORD)).expect("complete record");
    let repeated = TagPrerequisite {
        tag_id: record.prerequisites[0].tag_id.clone(),
        tag_object: "2".repeat(40),
    };
    assert_ne!(repeated, record.prerequisites[0]);
    record.prerequisites.push(repeated.clone());
    match record.encode() {
        Err(Error::Validation(detail)) => assert_eq!(
            detail,
            format!(
                "tag record names prerequisite {} more than once",
                repeated.tag_id
            )
        ),
        other => panic!("expected a validation failure, got {other:?}"),
    }
}
