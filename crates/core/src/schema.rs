// ---
// relationships:
//   implements:
//     - release-plan
//     - tag-record
// ---

//! Validation of durable records against their published schemas.
//!
//! The schemas are the YAML files in `docs/specifications`, embedded at build
//! time. Cross-schema references resolve against the embedded set only.

use crate::error::{Error, Result};
use jsonschema::{Registry, Validator};
use serde_json::{json, Value};
use std::fmt;
use std::sync::LazyLock;

const RELEASE_WORKSPACE_SCHEMA: &str =
    include_str!(concat!(env!("OUT_DIR"), "/release-workspace.schema.yml"));
const RELEASE_PLAN_SCHEMA: &str =
    include_str!(concat!(env!("OUT_DIR"), "/release-plan.schema.yml"));
const TAG_RECORD_SCHEMA: &str = include_str!(concat!(env!("OUT_DIR"), "/tag-record.schema.yml"));

/// A durable record kind with a published schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaRecord {
    /// A sealed Release Plan.
    ReleasePlan,
    /// The protocol fields of an annotated tag message.
    TagRecord,
    /// The payload whose digest a baseline tag record carries.
    BaselineDigestPayload,
}

impl fmt::Display for SchemaRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReleasePlan => "Release Plan",
            Self::TagRecord => "Tag Record",
            Self::BaselineDigestPayload => "baseline digest payload",
        })
    }
}

struct Validators {
    release_plan: Validator,
    tag_record: Validator,
    baseline_digest_payload: Validator,
}

static VALIDATORS: LazyLock<std::result::Result<Validators, String>> =
    LazyLock::new(compile_validators);

/// Parse one embedded schema and return it with its `$id`.
fn published(text: &str) -> std::result::Result<(String, Value), String> {
    let schema: Value =
        serde_yaml::from_str(text).map_err(|error| format!("schema is not YAML: {error}"))?;
    let id = schema["$id"]
        .as_str()
        .ok_or_else(|| "schema has no $id".to_owned())?
        .to_owned();
    Ok((id, schema))
}

fn compile_validators() -> std::result::Result<Validators, String> {
    let workspace = published(RELEASE_WORKSPACE_SCHEMA)?;
    let plan = published(RELEASE_PLAN_SCHEMA)?;
    let tag = published(TAG_RECORD_SCHEMA)?;
    let (plan_id, tag_id) = (plan.0.clone(), tag.0.clone());
    let mut registry = Registry::new();
    for (id, schema) in [workspace, plan, tag] {
        registry = registry
            .add(id, schema)
            .map_err(|error| format!("schema does not register: {error}"))?;
    }
    let registry = registry
        .prepare()
        .map_err(|error| format!("schema registry does not resolve: {error}"))?;
    let build = |reference: String| {
        jsonschema::options()
            .offline()
            .with_registry(&registry)
            .build(&json!({ "$ref": reference }))
            .map_err(|error| format!("schema {reference} does not compile: {error}"))
    };
    Ok(Validators {
        release_plan: build(plan_id)?,
        tag_record: build(tag_id.clone())?,
        baseline_digest_payload: build(format!("{tag_id}#/$defs/baseline-digest-payload"))?,
    })
}

/// Validate `instance` against the published schema of `record`.
///
/// The first failure becomes [`Error::Schema`], which names the record, the
/// failing location, and the schema keyword.
pub fn validate(record: SchemaRecord, instance: &Value) -> Result<()> {
    let validators = VALIDATORS
        .as_ref()
        .map_err(|reason| Error::Validation(format!("published schemas are unusable: {reason}")))?;
    let validator = match record {
        SchemaRecord::ReleasePlan => &validators.release_plan,
        SchemaRecord::TagRecord => &validators.tag_record,
        SchemaRecord::BaselineDigestPayload => &validators.baseline_digest_payload,
    };
    match validator.validate(instance) {
        Ok(()) => Ok(()),
        Err(error) => Err(Error::Schema {
            record,
            subject: None,
            location: error.instance_path().to_string(),
            keyword: error.kind().keyword().to_owned(),
            detail: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_published_record_schema_compiles() {
        assert!(VALIDATORS.is_ok(), "{:?}", VALIDATORS.as_ref().err());
    }

    #[test]
    fn embedded_schemas_are_the_published_files() {
        for (embedded, published) in [
            (
                RELEASE_WORKSPACE_SCHEMA,
                include_str!("../../../docs/specifications/release-workspace.schema.yml"),
            ),
            (
                RELEASE_PLAN_SCHEMA,
                include_str!("../../../docs/specifications/release-plan.schema.yml"),
            ),
            (
                TAG_RECORD_SCHEMA,
                include_str!("../../../docs/specifications/tag-record.schema.yml"),
            ),
        ] {
            assert_eq!(embedded, published);
        }
    }

    #[test]
    fn release_plan_schema_identity_matches_the_written_identity() {
        let (id, _) = published(RELEASE_PLAN_SCHEMA).expect("Release Plan schema");
        assert_eq!(id, crate::plan::RELEASE_PLAN_SCHEMA);
    }

    #[test]
    fn schema_failure_names_record_location_and_keyword() {
        let error = validate(SchemaRecord::ReleasePlan, &json!({ "digest": "sha256:0" }))
            .expect_err("incomplete plan");
        let Error::Schema {
            record,
            location,
            keyword,
            ..
        } = &error
        else {
            panic!("expected a schema error, got {error}");
        };
        assert_eq!(*record, SchemaRecord::ReleasePlan);
        assert_eq!(keyword, "required");
        assert_eq!(location, "");
        assert!(error
            .to_string()
            .starts_with("Release Plan violates its published schema at #: keyword `required`"));
    }
}
