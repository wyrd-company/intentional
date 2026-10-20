// ---
// relationships:
//   tests: release-model
// ---

//! The Release Model schema against its published fixtures.

use jsonschema::{Draft, Validator};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const RELEASE_MODEL_ID: &str = "https://intentional.foo/schemas/release-model/1";

/// Each invalid fixture with the instance location and keyword of the one
/// violation it carries.
const INVALID: [(&str, &str, &str); 16] = [
    ("wrong-schema", "/$schema", "const"),
    (
        "known-ecosystem-marked-generic",
        "/release_units/atlas-cli/projections/1/generic",
        "const",
    ),
    (
        "unlisted-generic-without-pointer",
        "/release_units/atlas-cli/projections/1",
        "required",
    ),
    (
        "unlisted-generic-native-identity",
        "/release_units/atlas-cli/projections/1",
        "not",
    ),
    (
        "known-generic-marked-ecosystem",
        "/release_units/atlas-cli/projections/1/generic",
        "const",
    ),
    (
        "missing-generic",
        "/release_units/atlas-cli/projections/1",
        "required",
    ),
    (
        "generic-not-boolean",
        "/release_units/atlas-cli/projections/1/generic",
        "type",
    ),
    (
        "release-record-value",
        "/release_units/atlas/release_record",
        "enum",
    ),
    (
        "disposition-value",
        "/release_units/atlas/disposition",
        "enum",
    ),
    (
        "tag-role-value",
        "/release_units/atlas/tags/primary/role",
        "enum",
    ),
    (
        "null-optional-member",
        "/release_units/atlas/tags/primary/require_phase",
        "enum",
    ),
    ("missing-required-list", "/release_units/atlas", "required"),
    (
        "mode-value",
        "/release_units/atlas/projections/0/mode",
        "enum",
    ),
    (
        "require-phase-value",
        "/release_units/atlas/tags/mirror/require_phase",
        "enum",
    ),
    (
        "generic-native-identity",
        "/release_units/atlas-cli/projections/1",
        "not",
    ),
    (
        "generic-without-pointer",
        "/release_units/atlas-cli/projections/1",
        "required",
    ),
];

fn specifications() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/specifications")
}

fn fixtures() -> PathBuf {
    specifications().join("fixtures/release-model")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn schema() -> Value {
    serde_yaml::from_str(&read(&specifications().join("release-model.schema.yml")))
        .expect("schema YAML parses as JSON-compatible data")
}

fn validator() -> Validator {
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema())
        .expect("validator builds")
}

/// The `.json` fixture names directly inside `directory`, without extension.
fn fixture_names(directory: &Path) -> BTreeSet<String> {
    fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.is_file())
        .map(|path| {
            assert_eq!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("json"),
                "{} is not a JSON fixture",
                path.display()
            );
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .expect("UTF-8 fixture name")
                .to_owned()
        })
        .collect()
}

fn document(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every instance location and keyword the schema reports for `instance`.
fn violations(validator: &Validator, instance: &Value) -> Vec<(String, String)> {
    validator
        .iter_errors(instance)
        .map(|error| {
            (
                error.instance_path().to_string(),
                error.kind().keyword().to_owned(),
            )
        })
        .collect()
}

#[test]
fn schema_has_the_release_model_identity_and_is_self_contained() {
    let schema = schema();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["$id"], RELEASE_MODEL_ID);
    assert_eq!(schema["properties"]["$schema"]["const"], RELEASE_MODEL_ID);

    let mut references = Vec::new();
    collect_references(&schema, &mut references);
    assert!(!references.is_empty(), "schema uses local definitions");
    for reference in references {
        assert!(
            reference.starts_with("#/$defs/"),
            "{reference} leaves the schema, so a vendored copy cannot resolve it"
        );
    }
}

fn collect_references(value: &Value, references: &mut Vec<String>) {
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                match (name.as_str(), member) {
                    ("$ref", Value::String(reference)) => references.push(reference.clone()),
                    _ => collect_references(member, references),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_references(item, references);
            }
        }
        _ => {}
    }
}

#[test]
fn valid_fixtures_validate_and_are_canonical() {
    let validator = validator();
    let names = fixture_names(&fixtures());
    for required in [
        "complete",
        "unknown-members",
        "unlisted-adapter",
        "unlisted-generic-adapter",
    ] {
        assert!(names.contains(required), "missing valid fixture {required}");
    }
    for name in &names {
        let path = fixtures().join(format!("{name}.json"));
        let text = read(&path);
        let instance = document(&path);
        let errors = violations(&validator, &instance);
        assert!(errors.is_empty(), "{name} is rejected: {errors:#?}");
        assert_eq!(
            format!(
                "{}\n",
                serde_json::to_string_pretty(&instance).expect("serialize")
            ),
            text,
            "{name} is not in canonical form"
        );
    }
}

#[test]
fn complete_fixture_covers_every_shape_the_contract_names() {
    let model = document(&fixtures().join("complete.json"));
    let units = model["release_units"].as_object().expect("release units");
    let values = |member: &str| -> BTreeSet<&str> {
        units
            .values()
            .filter_map(|unit| unit[member].as_str())
            .collect()
    };
    assert_eq!(
        values("disposition"),
        BTreeSet::from(["managed", "suspended"])
    );
    assert_eq!(
        values("release_record"),
        BTreeSet::from(["required", "none"])
    );
    assert!(units
        .values()
        .any(|unit| unit["projections"] == Value::Array(Vec::new())));
    assert!(units
        .values()
        .any(|unit| unit.get("release_group").is_none()));
    assert!(units
        .values()
        .flat_map(|unit| unit["projections"].as_array().expect("projections"))
        .any(|projection| projection["adapter"] == "yaml"
            && projection.get("pointer").is_some()
            && projection.get("native_identity").is_none()));
    for kind in ["fixed", "linked"] {
        assert!(!model["release_groups"][kind]
            .as_array()
            .expect("group list")
            .is_empty());
    }
    assert!(!model["workspace_tags"]
        .as_object()
        .expect("workspace tags")
        .is_empty());
}

#[test]
fn unknown_members_are_admitted_in_every_object() {
    let model = document(&fixtures().join("unknown-members.json"));
    let atlas = &model["release_units"]["atlas"];
    for object in [
        &model,
        &model["paths"],
        atlas,
        &atlas["release_group"],
        &atlas["projections"][0],
        &atlas["sources"][0],
        &atlas["tags"]["primary"],
        &model["workspace_tags"]["release"],
        &model["release_groups"],
    ] {
        assert!(
            object.get("extension_note").is_some(),
            "fixture carries an unknown member in {object}"
        );
    }
}

#[test]
fn invalid_fixtures_fail_for_their_stated_violation() {
    let validator = validator();
    let directory = fixtures().join("invalid");
    let expected: BTreeSet<String> = INVALID
        .iter()
        .map(|(name, ..)| (*name).to_owned())
        .collect();
    assert_eq!(
        fixture_names(&directory),
        expected,
        "invalid fixtures on disk and their stated violations differ"
    );
    for (name, location, keyword) in INVALID {
        let instance = document(&directory.join(format!("{name}.json")));
        let errors = violations(&validator, &instance);
        assert_eq!(
            errors,
            vec![(location.to_owned(), keyword.to_owned())],
            "{name} must fail with exactly {keyword} at {location}"
        );
    }
}

/// Required members of each object, entry by entry from the Release Model
/// design; every other defined member is optional.
#[test]
fn required_members_match_the_design() {
    let schema = schema();
    let required = |object: &Value| -> Vec<String> {
        object["required"]
            .as_array()
            .expect("required list")
            .iter()
            .map(|member| member.as_str().expect("member name").to_owned())
            .collect()
    };
    let definition = |name: &str| schema["$defs"][name].clone();
    for (object, members) in [
        (
            schema.clone(),
            &[
                "$schema",
                "paths",
                "release_units",
                "workspace_tags",
                "release_groups",
            ][..],
        ),
        (definition("paths"), &["config", "intents", "model"]),
        (
            definition("release-unit"),
            &[
                "path",
                "changelog",
                "disposition",
                "release_record",
                "depends_on",
                "ecosystems",
                "projections",
                "sources",
                "tags",
            ],
        ),
        (definition("release-group-membership"), &["kind", "members"]),
        (
            definition("projection"),
            &["adapter", "file", "workspace_file", "mode", "generic"],
        ),
        (definition("source"), &["detector", "path"]),
        (
            definition("release-unit-tag"),
            &["role", "template", "tag_after"],
        ),
        (definition("workspace-tag"), &["template", "tag_after"]),
        (definition("release-groups"), &["fixed", "linked"]),
    ] {
        assert_eq!(required(&object), members, "{}", object["properties"]);
    }
}

/// `adapter` and `ecosystems` are open: an adapter outside the known values
/// stays within line 1.
#[test]
fn unlisted_adapter_fixture_names_an_adapter_outside_the_known_values() {
    let known = [
        "npm", "cargo", "pub", "python", "msbuild", "go", "json", "toml", "yaml",
    ];
    let schema = schema();
    assert_eq!(
        schema["$defs"]["adapter"]["examples"],
        serde_json::json!(known)
    );
    let delta = &document(&fixtures().join("unlisted-adapter.json"))["release_units"]["delta"];
    let adapter = delta["projections"][0]["adapter"]
        .as_str()
        .expect("adapter");
    assert!(!known.contains(&adapter), "{adapter} is a known adapter");
    assert_eq!(delta["ecosystems"], serde_json::json!([adapter]));
    assert_eq!(delta["projections"][0]["generic"], false);
    assert!(delta["projections"][0].get("pointer").is_none());
}

fn collect_enums(value: &Value, location: &str, enums: &mut BTreeSet<String>) {
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                let path = format!("{location}/{name}");
                if name == "enum" {
                    enums.insert(path);
                } else {
                    collect_enums(member, &path, enums);
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_enums(item, &format!("{location}/{index}"), enums);
            }
        }
        _ => {}
    }
}

/// Every enumerated member is closed except `adapter` and each `ecosystems`
/// item; `generic-adapter` identifies known adapters that must declare generic.
#[test]
fn closed_value_sets_are_exactly_the_ruled_ones() {
    let schema = schema();
    let mut enums = BTreeSet::new();
    collect_enums(&schema, "", &mut enums);
    let expected: BTreeSet<String> = [
        "/$defs/generic-adapter/enum",
        "/$defs/projection/properties/mode/enum",
        "/$defs/release-group-membership/properties/kind/enum",
        "/$defs/release-unit-tag/properties/role/enum",
        "/$defs/release-unit/properties/disposition/enum",
        "/$defs/release-unit/properties/release_record/enum",
        "/$defs/tag-phase/enum",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(enums, expected);
    assert_eq!(
        schema["$defs"]["projection"]["properties"]["adapter"]["$ref"],
        "#/$defs/adapter"
    );
    assert_eq!(
        schema["$defs"]["release-unit"]["properties"]["ecosystems"]["items"]["$ref"],
        "#/$defs/adapter"
    );
}

/// The specification, the schema, and the semantic-versioning design state the
/// open and closed value rule in the same words.
#[test]
fn open_and_closed_value_rule_reads_the_same_everywhere() {
    let rule = "`adapter` and each `ecosystems` item are the only open values in the \
                Release Model: a new adapter stays within the line. Every other \
                enumerated member, `mode`, `require_phase`, `disposition`, \
                `release_record`, `role`, and `kind`, is closed, and a new value for \
                any of them mints the next line.";
    let normalize = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let specification: Value =
        serde_yaml::from_str(&read(&specifications().join("release-model.yml"))).expect("spec");
    let design: Value = serde_yaml::from_str(&read(
        &specifications().join("../technical-designs/semantic-versioning.yml"),
    ))
    .expect("design");
    for (source, text) in [
        ("schema description", schema()["description"].clone()),
        (
            "specification",
            specification["validation-criteria"].clone(),
        ),
        (
            "semantic-versioning design",
            design["proposed-design"]["published-schema-stability"].clone(),
        ),
    ] {
        let text = text.as_str().unwrap_or_else(|| panic!("{source} text"));
        assert!(
            normalize(text).contains(rule),
            "{source} does not state the rule"
        );
    }
}

/// A new generic adapter uses the same discriminator as a known one. Its
/// missing-pointer fixture differs only by the absent pointer.
#[test]
fn unlisted_generic_adapter_requires_pointer() {
    let mut valid = document(&fixtures().join("unlisted-generic-adapter.json"));
    let projection = &mut valid["release_units"]["atlas-cli"]["projections"][1];
    let adapter = projection["adapter"].as_str().expect("adapter");
    assert!(!schema()["$defs"]["adapter"]["examples"]
        .as_array()
        .expect("known adapters")
        .iter()
        .any(|known| known == adapter));
    assert_eq!(projection["generic"], true);
    assert!(projection
        .as_object_mut()
        .expect("projection")
        .remove("pointer")
        .is_some());
    assert_eq!(
        valid,
        document(&fixtures().join("invalid/unlisted-generic-without-pointer.json"))
    );
    assert_eq!(
        violations(&validator(), &valid),
        vec![(
            "/release_units/atlas-cli/projections/1".to_owned(),
            "required".to_owned()
        )]
    );
}

#[test]
fn known_generic_adapters_require_generic_true() {
    let validator = validator();
    for adapter in ["json", "toml", "yaml"] {
        let mut model = document(&fixtures().join("complete.json"));
        model["release_units"]["atlas-cli"]["projections"][1]["adapter"] = adapter.into();
        assert!(violations(&validator, &model).is_empty(), "{adapter}");
        model["release_units"]["atlas-cli"]["projections"][1]["generic"] = false.into();
        assert_eq!(
            violations(&validator, &model),
            vec![(
                "/release_units/atlas-cli/projections/1/generic".to_owned(),
                "const".to_owned()
            )],
            "{adapter}"
        );
    }
}

/// A pointer and absent native identity leave classification as the sole
/// violation. Ecosystem projections need neither pointer nor native identity.
#[test]
fn known_ecosystem_adapters_require_generic_false() {
    let validator = validator();
    for adapter in ["npm", "cargo", "pub", "python", "msbuild", "go"] {
        let mut model = document(&fixtures().join("complete.json"));
        let projection = &mut model["release_units"]["atlas-cli"]["projections"][1];
        projection["adapter"] = adapter.into();
        assert_eq!(projection["generic"], true);
        assert!(projection["pointer"].is_string());
        assert!(projection.get("native_identity").is_none());
        if adapter == "npm" {
            assert_eq!(
                model,
                document(&fixtures().join("invalid/known-ecosystem-marked-generic.json"))
            );
        }
        assert_eq!(
            violations(&validator, &model),
            vec![(
                "/release_units/atlas-cli/projections/1/generic".to_owned(),
                "const".to_owned()
            )],
            "{adapter}"
        );
        let projection = &mut model["release_units"]["atlas-cli"]["projections"][1];
        projection["generic"] = false.into();
        assert!(violations(&validator, &model).is_empty(), "{adapter}");
        model["release_units"]["atlas-cli"]["projections"][1]
            .as_object_mut()
            .expect("projection")
            .remove("pointer");
        assert!(violations(&validator, &model).is_empty(), "{adapter}");
    }
}
