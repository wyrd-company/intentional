// ---
// relationships:
//   tests: release-plan
// ---

//! The Release Plan codec against the published fixtures.

use intentional_core::schema::SchemaRecord;
use intentional_core::{Error, ReleasePlan, TagRole};
use std::fs;
use std::path::PathBuf;

const RELEASED_0_2_0_PLAN: &str = "released-0.2.0/release-plan.json";
const CANONICAL_PLAN: &str = "release-plan/canonical.json";
const REORDERED_PLAN: &str = "release-plan/reordered.json";

fn fixture(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/specifications/fixtures")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The canonical document of a fixture written as `intentional plan` prints
/// it: the encoded bytes followed by one newline.
fn canonical_bytes(text: &str) -> &str {
    text.strip_suffix('\n')
        .expect("fixture ends with a newline")
}

#[test]
fn canonical_release_plans_round_trip_to_their_bytes() {
    for name in [RELEASED_0_2_0_PLAN, CANONICAL_PLAN] {
        let text = fixture(name);
        let plan = ReleasePlan::decode(&text).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            plan.encode().expect("encode"),
            canonical_bytes(&text),
            "{name}"
        );
    }
}

#[test]
fn release_plan_written_by_released_0_2_0_decodes_without_a_schema_identity() {
    let plan = ReleasePlan::decode(&fixture(RELEASED_0_2_0_PLAN)).expect("released plan");
    assert_eq!(plan.schema, None);
    assert_eq!(plan.source, None);
    assert_eq!(plan.generator.version, "0.2.0");
}

#[test]
fn fixed_release_plans_carry_their_recorded_digests() {
    for (name, digest) in [
        (
            RELEASED_0_2_0_PLAN,
            "sha256:cd4f1ebfe3db2b887ecfd646383c5887e48b261a4e7512efb02289ee867120e2",
        ),
        (
            CANONICAL_PLAN,
            "sha256:5d12df2da6da303081635e34f1edde2526305e3ce6434bb4d8676228c0df4c22",
        ),
    ] {
        let plan = ReleasePlan::decode(&fixture(name)).expect("fixed plan");
        assert_eq!(plan.digest, digest, "{name}");
    }
}

#[test]
fn reordered_keys_decode_to_the_same_digest_and_canonical_bytes() {
    let canonical = ReleasePlan::decode(&fixture(CANONICAL_PLAN)).expect("canonical plan");
    let reordered = ReleasePlan::decode(&fixture(REORDERED_PLAN)).expect("reordered plan");
    assert_eq!(reordered, canonical);
    assert_eq!(
        reordered.encode().expect("encode"),
        canonical_bytes(&fixture(CANONICAL_PLAN))
    );
}

#[test]
fn the_digest_seals_every_member_but_itself() {
    let text = fixture(CANONICAL_PLAN);
    let other_digest = format!("\"digest\":\"sha256:{}\"", "0".repeat(64));
    let resealed = text.replacen(
        "\"digest\":\"sha256:5d12df2da6da303081635e34f1edde2526305e3ce6434bb4d8676228c0df4c22\"",
        &other_digest,
        1,
    );
    assert_ne!(resealed, text);
    let error = ReleasePlan::decode(&resealed).expect_err("foreign digest");
    assert!(
        error.to_string().contains("release plan digest mismatch"),
        "{error}"
    );

    let edited = text.replacen("Add a sample capability.", "Add another capability.", 1);
    assert_ne!(edited, text);
    let error = ReleasePlan::decode(&edited).expect_err("edited payload");
    assert!(
        error.to_string().contains("release plan digest mismatch"),
        "{error}"
    );
}

#[test]
fn invalid_release_plans_fail_at_the_codec_for_their_stated_reason() {
    for (name, expected_location, expected_keyword) in [
        ("undeclared-member", "", "additionalProperties"),
        ("wrong-schema", "/$schema", "const"),
        ("digest-pattern", "/digest", "pattern"),
        ("semver-pattern", "/release_units/0/new_version", "pattern"),
        ("git-object-pattern", "/source/commit", "pattern"),
        ("workspace-tag-role", "/tags/2", "not"),
        ("missing-required", "", "required"),
    ] {
        let text = fixture(&format!("release-plan/invalid/{name}.json"));
        match ReleasePlan::decode(&text) {
            Err(Error::Schema {
                record,
                location,
                keyword,
                ..
            }) => {
                assert_eq!(record, SchemaRecord::ReleasePlan, "{name}");
                assert_eq!(location, expected_location, "{name}");
                assert_eq!(keyword, expected_keyword, "{name}");
            }
            other => panic!("{name}: expected a schema failure, got {other:?}"),
        }
    }
}

#[test]
fn encoding_refuses_a_plan_its_schema_rejects() {
    let mut plan = ReleasePlan::decode(&fixture(CANONICAL_PLAN)).expect("canonical plan");
    let workspace = plan
        .tags
        .iter_mut()
        .find(|tag| tag.id == "workspace/release")
        .expect("workspace tag");
    workspace.role = Some(TagRole::Primary);
    match plan.encode() {
        Err(Error::Schema {
            record,
            location,
            keyword,
            ..
        }) => {
            assert_eq!(record, SchemaRecord::ReleasePlan);
            assert_eq!(location, "/tags/2");
            assert_eq!(keyword, "not");
        }
        other => panic!("expected a schema failure, got {other:?}"),
    }
}
