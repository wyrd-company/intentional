// ---
// relationships:
//   implements: tag-record
// ---

//! The tag record: the protocol message of an Intentional annotated tag.
//!
//! A message is the protocol header, one blank line, and one `key: value` line
//! per field. Scalar fields occur once in schema order; `prerequisite` lines
//! follow in lexical tag ID order and project into the `prerequisites` array
//! that the published Tag Record schema validates.

use crate::error::{Error, Result};
use crate::plan::{canonical_json, sha256_digest};
use crate::schema::{self, SchemaRecord};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// First line of every tag record message.
pub const TAG_RECORD_HEADER: &str = "intentional release record";

/// Scalar fields in schema order.
const SCALAR_FIELDS: [&str; 8] = [
    "contract",
    "generator",
    "plan-digest",
    "tag-id",
    "tag-role",
    "version",
    "target",
    "baseline",
];

/// Line key of one prerequisite entry.
const PREREQUISITE_FIELD: &str = "prerequisite";

/// Schema member that collects the prerequisite entries.
const PREREQUISITES_MEMBER: &str = "prerequisites";

/// Role of the tag a tag record occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TagRecordRole {
    /// Primary Release Unit tag.
    Primary,
    /// Projection Release Unit tag.
    Projection,
    /// Workspace tag.
    Workspace,
}

/// One verified prerequisite tag object.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TagPrerequisite {
    /// Canonical tag ID of the prerequisite.
    pub tag_id: String,
    /// Object ID of the prerequisite annotated tag.
    pub tag_object: String,
}

/// The protocol fields of one tag record.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TagRecord {
    /// Interpretation contract.
    pub contract: String,
    /// Producing generator: the tool name `intentional`, one space, and its
    /// semantic version.
    pub generator: String,
    /// Sealed Release Plan digest, or the baseline digest for a baseline.
    pub plan_digest: String,
    /// Canonical configured tag ID.
    pub tag_id: String,
    /// Role of the tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_role: Option<TagRecordRole>,
    /// Version the tag records.
    pub version: String,
    /// Target commit object ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Whether the record establishes a baseline.
    pub baseline: bool,
    /// Verified prerequisite tag objects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<TagPrerequisite>,
}

impl TagRecord {
    /// A record with the required fields only, as `intentional tag` writes it.
    pub fn new(
        contract: &str,
        generator_version: &str,
        plan_digest: &str,
        tag_id: &str,
        version: &str,
        baseline: bool,
    ) -> Self {
        Self {
            contract: contract.to_owned(),
            generator: format!("intentional {generator_version}"),
            plan_digest: plan_digest.to_owned(),
            tag_id: tag_id.to_owned(),
            tag_role: None,
            version: version.to_owned(),
            target: None,
            baseline,
            prerequisites: Vec::new(),
        }
    }

    /// Whether `message` claims to carry a tag record: it begins with the
    /// protocol header, or one of its lines names a tag record field. A message
    /// that makes no claim is history that predates Intentional.
    pub fn is_claimed_by(message: &str) -> bool {
        message.starts_with(TAG_RECORD_HEADER)
            || message.lines().any(|line| {
                line.split_once(": ").is_some_and(|(key, _)| {
                    key == PREREQUISITE_FIELD || SCALAR_FIELDS.contains(&key)
                })
            })
    }

    /// Encode the canonical message. The fields must satisfy the published Tag
    /// Record schema, and each prerequisite tag ID must occur once.
    pub fn encode(&self) -> Result<String> {
        let mut record = self.clone();
        record
            .prerequisites
            .sort_by(|left, right| left.tag_id.cmp(&right.tag_id));
        let fields = serde_json::to_value(&record).map_err(|error| {
            Error::Validation(format!("tag record serialization failed: {error}"))
        })?;
        schema::validate(SchemaRecord::TagRecord, &fields)?;
        require_unique_prerequisites(&record.prerequisites)?;
        let mut message = format!("{TAG_RECORD_HEADER}\n\n");
        for key in SCALAR_FIELDS {
            match &fields[key] {
                Value::String(value) => message.push_str(&format!("{key}: {value}\n")),
                Value::Bool(value) => message.push_str(&format!("{key}: {value}\n")),
                _ => {}
            }
        }
        for prerequisite in &record.prerequisites {
            message.push_str(&format!(
                "{PREREQUISITE_FIELD}: {} {}\n",
                prerequisite.tag_id, prerequisite.tag_object
            ));
        }
        Ok(message)
    }

    /// Decode a tag record message. The message must be canonical, and its
    /// fields must satisfy the published Tag Record schema. The final newline
    /// is optional, because Git drops it when a signature follows the message.
    /// Decoding re-encodes the record, so it refuses what encoding refuses.
    pub fn decode(message: &str) -> Result<Self> {
        let fields = project(message)?;
        schema::validate(SchemaRecord::TagRecord, &fields)?;
        let record: Self = serde_json::from_value(fields)
            .map_err(|error| Error::Validation(format!("invalid tag record: {error}")))?;
        let canonical = record.encode()?;
        if message.strip_suffix('\n').unwrap_or(message) != canonical.trim_end_matches('\n') {
            return Err(Error::Validation(
                "tag record is not canonical: fields must occur once in schema order, followed by prerequisites in tag ID order"
                    .to_owned(),
            ));
        }
        Ok(record)
    }
}

/// Refuse a prerequisite tag ID that occurs more than once. The schema's
/// `uniqueItems` refuses only identical entries, not one tag ID paired with
/// two tag objects.
fn require_unique_prerequisites(prerequisites: &[TagPrerequisite]) -> Result<()> {
    let mut tag_ids = BTreeSet::new();
    match prerequisites
        .iter()
        .find(|prerequisite| !tag_ids.insert(prerequisite.tag_id.as_str()))
    {
        Some(repeated) => Err(Error::Validation(format!(
            "tag record names prerequisite {} more than once",
            repeated.tag_id
        ))),
        None => Ok(()),
    }
}

/// Project a message onto the object the Tag Record schema validates.
fn project(message: &str) -> Result<Value> {
    let body = message
        .strip_prefix(TAG_RECORD_HEADER)
        .and_then(|rest| rest.strip_prefix("\n\n"))
        .ok_or_else(|| {
            Error::Validation(format!(
                "tag record does not begin with the protocol header `{TAG_RECORD_HEADER}` and a blank line"
            ))
        })?;
    let mut fields = Map::new();
    let mut prerequisites = Vec::new();
    for line in body.lines() {
        let (key, value) = line.split_once(": ").ok_or_else(|| {
            Error::Validation(format!(
                "tag record line `{line}` is not a `key: value` field"
            ))
        })?;
        if key == PREREQUISITE_FIELD {
            prerequisites.push(match value.split_once(' ') {
                Some((tag_id, tag_object)) => json!({ "tag-id": tag_id, "tag-object": tag_object }),
                None => Value::String(value.to_owned()),
            });
            continue;
        }
        let value = match (key, value) {
            ("baseline", "true") => Value::Bool(true),
            ("baseline", "false") => Value::Bool(false),
            _ => Value::String(value.to_owned()),
        };
        if key == PREREQUISITES_MEMBER || fields.insert(key.to_owned(), value).is_some() {
            return Err(Error::Validation(format!("tag record repeats field {key}")));
        }
    }
    if !prerequisites.is_empty() {
        fields.insert(PREREQUISITES_MEMBER.to_owned(), Value::Array(prerequisites));
    }
    Ok(Value::Object(fields))
}

/// Digest a baseline tag record carries: the SHA-256 digest of the canonical
/// baseline digest payload for `versions`, keyed by Release Unit ID and
/// canonical workspace tag ID.
pub fn baseline_digest(
    contract: &str,
    generator_version: &str,
    versions: &BTreeMap<String, String>,
) -> Result<String> {
    let payload = json!({
        "contract": contract,
        "generator": generator_version,
        "versions": versions,
        "baseline": true,
    });
    schema::validate(SchemaRecord::BaselineDigestPayload, &payload)?;
    Ok(sha256_digest(canonical_json(&payload)?.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published_schema() -> serde_yaml::Value {
        serde_yaml::from_str(include_str!(
            "../../../docs/specifications/tag-record.schema.yml"
        ))
        .expect("Tag Record schema parses")
    }

    #[test]
    fn scalar_fields_follow_the_published_schema_order() {
        let schema = published_schema();
        let properties: Vec<&str> = schema["properties"]
            .as_mapping()
            .expect("Tag Record properties")
            .keys()
            .filter_map(serde_yaml::Value::as_str)
            .collect();
        let mut expected = SCALAR_FIELDS.to_vec();
        expected.push(PREREQUISITES_MEMBER);
        assert_eq!(properties, expected);
    }

    #[test]
    fn published_schema_requires_the_fields_intentional_writes() {
        let schema = published_schema();
        assert_eq!(
            schema["$id"].as_str(),
            Some("https://intentional.foo/schemas/0.2/tag-record")
        );
        let required: Vec<&str> = schema["required"]
            .as_sequence()
            .expect("required Tag Record fields")
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .collect();
        assert_eq!(
            required,
            [
                "contract",
                "generator",
                "plan-digest",
                "tag-id",
                "version",
                "baseline"
            ]
        );
        assert_eq!(
            schema["properties"]["contract"]["const"].as_str(),
            Some(crate::config::CURRENT_CONTRACT)
        );
    }

    #[test]
    fn a_message_without_any_record_field_makes_no_claim() {
        assert!(!TagRecord::is_claimed_by("Release 1.0.0\n"));
        assert!(TagRecord::is_claimed_by("contract: contract-1\n"));
        assert!(TagRecord::is_claimed_by(&format!("{TAG_RECORD_HEADER}\n")));
    }
}
