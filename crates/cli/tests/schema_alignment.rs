// ---
// relationships:
//   tests:
//     - release-workspace
//     - tag-record
// ---

//! Validates what Intentional reads and writes against the published schemas.

use assert_cmd::Command;
use intentional_core::{canonical_json, sha256_digest, Config};
use jsonschema::{Registry, Validator};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

const CONFIG_ID: &str = "https://intentional.foo/schemas/0.2/config";
const TAG_RECORD_ID: &str = "https://intentional.foo/schemas/0.2/tag-record";
const BASELINE_PAYLOAD_ID: &str =
    "https://intentional.foo/schemas/0.2/tag-record#/$defs/baseline-digest-payload";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_repository_file(relative: &str) -> String {
    fs::read_to_string(repository_root().join(relative)).expect("repository file")
}

fn yaml_to_json(text: &str) -> Value {
    serde_yaml::from_str(text).expect("YAML parses as JSON-compatible data")
}

fn published_schema(name: &str) -> Value {
    yaml_to_json(&read_repository_file(&format!(
        "docs/specifications/{name}.schema.yml"
    )))
}

/// Build an offline validator for `reference` with every published schema
/// registered under its `$id`, so cross-schema references resolve locally.
fn validator(reference: &str) -> Validator {
    let mut registry = Registry::new();
    for name in ["release-workspace", "release-plan", "tag-record"] {
        let schema = published_schema(name);
        let id = schema["$id"].as_str().expect("schema $id").to_owned();
        registry = registry.add(id, schema).expect("schema registers");
    }
    let registry = registry.prepare().expect("registry prepares");
    jsonschema::options()
        .offline()
        .with_registry(&registry)
        .build(&json!({ "$ref": reference }))
        .expect("validator builds")
}

fn schema_errors(reference: &str, instance: &Value) -> Vec<String> {
    validator(reference)
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect()
}

fn assert_valid(reference: &str, instance: &Value) {
    let errors = schema_errors(reference, instance);
    assert!(
        errors.is_empty(),
        "{reference} rejects instance: {errors:#?}"
    );
}

fn assert_invalid(reference: &str, instance: &Value) {
    assert!(
        !schema_errors(reference, instance).is_empty(),
        "{reference} accepts instance {instance}"
    );
}

/// Project a tag record message onto the object the Tag Record schema
/// validates: scalar `key: value` lines, `baseline` as a boolean, and
/// repeated `prerequisite` lines as the `prerequisites` array.
fn tag_record_fields(message: &str) -> Value {
    let mut lines = message.lines();
    assert_eq!(lines.next(), Some("intentional release record"));
    assert_eq!(lines.next(), Some(""));
    let mut fields = serde_json::Map::new();
    let mut prerequisites = Vec::new();
    for line in lines {
        let (key, value) = line.split_once(": ").expect("tag record field line");
        match key {
            "baseline" => {
                fields.insert(key.to_owned(), json!(value.parse::<bool>().expect("bool")));
            }
            "prerequisite" => {
                let (tag_id, tag_object) = value.split_once(' ').expect("prerequisite pair");
                prerequisites.push(json!({ "tag-id": tag_id, "tag-object": tag_object }));
            }
            _ => {
                let previous = fields.insert(key.to_owned(), json!(value));
                assert!(previous.is_none(), "repeated tag record field {key}");
            }
        }
    }
    if !prerequisites.is_empty() {
        fields.insert("prerequisites".to_owned(), Value::Array(prerequisites));
    }
    Value::Object(fields)
}

struct TestRepo {
    _temp: TempDir,
    root: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("sample");
        fs::create_dir(&root).expect("workspace directory");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Fixture Author"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        Self { _temp: temp, root }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, contents).expect("fixture write");
    }

    fn commit(&self, message: &str) {
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-q", "-m", message]);
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(&self.root);
        command
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

#[test]
fn repository_configuration_validates_against_release_workspace_schema() {
    let text = read_repository_file(".intentional/config.yml");
    Config::from_yaml(&text).expect("repository configuration loads");
    assert_valid(CONFIG_ID, &yaml_to_json(&text));
}

#[test]
fn configuration_written_by_released_versions_loads_and_validates() {
    for fixture in [
        "released-0.1.2/config.yml",
        "released-0.1.8/config.yml",
        "released-0.2.0/config.yml",
        "released-0.2.0/release-config.yml",
    ] {
        let text = read_repository_file(&format!("docs/specifications/fixtures/{fixture}"));
        let config = Config::from_yaml(&text).unwrap_or_else(|error| panic!("{fixture}: {error}"));
        assert_eq!(
            config.schema.as_deref(),
            Some("https://intentional.foo/schemas/config.yml"),
            "{fixture}"
        );
        assert_valid(CONFIG_ID, &yaml_to_json(&text));
    }
}

#[test]
fn initialization_writes_the_compatibility_line_configuration_schema() {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.commit("add fixture");
    repo.cli().arg("init").assert().code(2);
    let plan_path = repo.root.join(".intentional/init-plan.yml");
    let mut plan: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("initialization plan"))
            .expect("initialization plan parses");
    plan["discovery-candidates"][0]["resolution"] =
        serde_yaml::from_str("{ kind: independent, release-unit: sample-library }")
            .expect("resolution parses");
    fs::write(
        &plan_path,
        serde_yaml::to_string(&plan).expect("resolved initialization plan"),
    )
    .expect("write resolved initialization plan");
    repo.cli().arg("init").assert().success();

    let text = fs::read_to_string(repo.root.join(".intentional/config.yml")).expect("config");
    let config = Config::from_yaml(&text).expect("written configuration loads");
    assert_eq!(config.schema.as_deref(), Some(CONFIG_ID));
    assert_valid(CONFIG_ID, &yaml_to_json(&text));
}

#[test]
fn release_workspace_schema_rejects_unpublished_identities_and_missing_contract() {
    let text = read_repository_file("docs/specifications/fixtures/released-0.2.0/config.yml");
    let mut config = yaml_to_json(&text);
    config["$schema"] = json!(CONFIG_ID);
    assert_valid(CONFIG_ID, &config);

    let mut unpublished = config.clone();
    unpublished["$schema"] = json!("https://intentional.foo/schemas/0.3/config");
    assert_invalid(CONFIG_ID, &unpublished);

    let mut without_contract = config.clone();
    without_contract
        .as_object_mut()
        .expect("config object")
        .remove("contract");
    assert_invalid(CONFIG_ID, &without_contract);
}

#[test]
fn tag_records_written_by_released_0_2_0_validate() {
    for fixture in [
        "release-unit-tag-record.txt",
        "workspace-tag-record.txt",
        "baseline-tag-record.txt",
    ] {
        let message = read_repository_file(&format!(
            "docs/specifications/fixtures/released-0.2.0/{fixture}"
        ));
        let fields = tag_record_fields(&message);
        assert_eq!(fields["contract"], "contract-1", "{fixture}");
        assert_valid(TAG_RECORD_ID, &fields);
    }
}

#[test]
fn baseline_digest_payload_written_by_released_0_2_0_validates_and_reproduces() {
    let payload: Value = serde_json::from_str(&read_repository_file(
        "docs/specifications/fixtures/released-0.2.0/baseline-digest-payload.json",
    ))
    .expect("baseline payload JSON");
    assert_valid(BASELINE_PAYLOAD_ID, &payload);
    let baseline = tag_record_fields(&read_repository_file(
        "docs/specifications/fixtures/released-0.2.0/baseline-tag-record.txt",
    ));
    assert_eq!(
        sha256_digest(canonical_json(&payload).expect("canonical").as_bytes()),
        baseline["plan-digest"].as_str().expect("baseline digest")
    );
}

#[test]
fn tag_record_schema_rejects_a_record_without_contract() {
    let message = read_repository_file(
        "docs/specifications/fixtures/released-0.2.0/workspace-tag-record.txt",
    );
    let mut fields = tag_record_fields(&message);
    fields.as_object_mut().expect("fields").remove("contract");
    assert_invalid(TAG_RECORD_ID, &fields);
    let mut workspace_as_primary = tag_record_fields(&message);
    workspace_as_primary["tag-role"] = json!("primary");
    assert_invalid(TAG_RECORD_ID, &workspace_as_primary);
}
