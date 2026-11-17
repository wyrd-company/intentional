// ---
// relationships:
//   tests: release-model
// ---

use intentional_core::{ReleaseModel, RELEASE_MODEL_SCHEMA_ID};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/specifications/fixtures/release-model")
            .join(name),
    )
    .unwrap()
}

#[test]
fn published_models_decode_and_encode_back_to_the_current_contract() {
    for name in [
        "complete.json",
        "unlisted-adapter.json",
        "unknown-members.json",
    ] {
        let model = ReleaseModel::decode(&fixture(name)).unwrap();
        let encoded = model.encode().unwrap();
        assert_eq!(ReleaseModel::decode(encoded.as_bytes()).unwrap(), model);
        assert_eq!(model.schema, RELEASE_MODEL_SCHEMA_ID);
        if name == "complete.json" {
            assert_eq!(encoded.as_bytes(), fixture(name));
        }
    }
}

#[test]
fn every_invalid_fixture_is_rejected_by_the_codec() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/specifications/fixtures/release-model/invalid");
    for path in fs::read_dir(directory).unwrap() {
        let path = path.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "json") {
            assert!(
                ReleaseModel::decode(&fs::read(&path).unwrap()).is_err(),
                "{}",
                path.display()
            );
        }
    }
    assert!(ReleaseModel::decode(b"{").is_err());
}

#[test]
fn encode_sorts_named_lists_without_changing_projection_order() {
    let model = ReleaseModel::decode(&fixture("complete.json")).unwrap();
    let expected = model.encode().unwrap();
    let mut reordered = model.clone();
    for unit in reordered.release_units.values_mut() {
        unit.depends_on.reverse();
        unit.ecosystems.reverse();
        unit.sources.reverse();
        if let Some(group) = &mut unit.release_group {
            group.members.reverse();
        }
        for tag in unit.tags.values_mut() {
            tag.tag_after.reverse();
        }
    }
    for tag in reordered.workspace_tags.values_mut() {
        tag.tag_after.reverse();
    }
    for groups in [
        &mut reordered.release_groups.fixed,
        &mut reordered.release_groups.linked,
    ] {
        groups.reverse();
        for members in groups {
            members.reverse();
        }
    }
    assert_eq!(reordered.encode().unwrap(), expected);
    let text = reordered.encode().unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    for (id, unit) in model.release_units {
        let expected: Value = serde_json::to_value(unit.projections).unwrap();
        assert_eq!(value["release_units"][id]["projections"], expected);
    }
}

#[test]
fn encode_enforces_schema_and_omits_absent_optionals_but_keeps_empty_lists() {
    let mut model = ReleaseModel::decode(&fixture("complete.json")).unwrap();
    model.schema.push('/');
    assert!(model.encode().is_err());
    model.schema = RELEASE_MODEL_SCHEMA_ID.to_owned();
    model.release_units.get_mut("atlas").unwrap().depends_on = vec!["atlas-cli".to_owned(); 2];
    assert!(model.encode().is_err());
    let value: Value = serde_json::from_slice(&fixture("complete.json")).unwrap();
    assert_eq!(value["release_units"]["atlas"]["depends_on"], json!([]));
    assert!(value["release_units"]["atlas"]["tags"]["primary"]
        .get("require_phase")
        .is_none());
}

#[test]
fn canonical_ordering_covers_each_named_list_and_source_tiebreaker() {
    let mut value: Value = serde_json::from_slice(&fixture("complete.json")).unwrap();
    value["release_groups"] = json!({"fixed":[["z","y"],["b","a"]],"linked":[["x","w"],["d","c"]]});
    let unit = &mut value["release_units"]["atlas"];
    unit["depends_on"] = json!(["z", "a"]);
    unit["ecosystems"] = json!(["python", "npm"]);
    unit["release_group"]["members"] = json!(["z", "a"]);
    unit["sources"] = json!([{"path":"z","detector":"a"},{"path":"a","detector":"z"},{"path":"a","detector":"a"}]);
    unit["tags"]["primary"]["tag_after"] = json!(["workspace/z", "workspace/a"]);
    for tag in value["workspace_tags"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        tag["tag_after"] = json!(["workspace/z", "workspace/a"]);
    }
    let model = ReleaseModel::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    let result: Value = serde_json::from_str(&model.encode().unwrap()).unwrap();
    let unit = &result["release_units"]["atlas"];
    assert_eq!(unit["depends_on"], json!(["a", "z"]));
    assert_eq!(unit["ecosystems"], json!(["npm", "python"]));
    assert_eq!(unit["release_group"]["members"], json!(["a", "z"]));
    assert_eq!(
        unit["sources"],
        json!([{"path":"a","detector":"a"},{"path":"a","detector":"z"},{"path":"z","detector":"a"}])
    );
    assert_eq!(
        unit["tags"]["primary"]["tag_after"],
        json!(["workspace/a", "workspace/z"])
    );
    for tag in result["workspace_tags"].as_object().unwrap().values() {
        assert_eq!(tag["tag_after"], json!(["workspace/a", "workspace/z"]));
    }
    assert_eq!(
        result["release_groups"],
        json!({"fixed":[["a","b"],["y","z"]],"linked":[["c","d"],["w","x"]]})
    );
}
