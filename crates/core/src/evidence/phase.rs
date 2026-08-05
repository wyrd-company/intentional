// ---
// relationships:
//   implements: github-release-executor
// ---

//! Canonical encoding of publication-phase evidence inside an annotated tag record.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::evidence::assemble::{
    EvidenceReference, IntendedDestination, PhaseSubject, PhaseTagEvidence, PublisherEvidence,
    PHASE_TAG_EVIDENCE_SCHEMA, PUBLISHER_EVIDENCE_SCHEMA,
};
use crate::evidence::{digest_bytes, digest_file, is_digest, is_git_object};
use crate::model::TagPhase;
use crate::plan::canonical_json;
use crate::release::tag::verify_release_tag;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Record field carrying phase evidence in an annotated tag message.
pub const PHASE_EVIDENCE_FIELD: &str = "phase-evidence";
/// Schema identity of one built-subject document.
pub const BUILT_SUBJECT_SCHEMA: &str = "https://intentional.foo/schemas/built-subject/v1";
/// Contract identity of one built-subject document.
pub const BUILT_SUBJECT_CONTRACT: &str = "built-subject-1";

/// One publishable subject the recipe's build job produced from the release commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BuiltSubject {
    /// Built-subject schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Built-subject contract identity.
    pub contract: String,
    /// Release unit the subject belongs to.
    pub release_unit: String,
    /// Subject identity at its destinations.
    pub identity: String,
    /// Built version.
    pub version: String,
    /// Immutable subject digest.
    pub digest: String,
    /// Native provenance bound to the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Vec<EvidenceReference>>,
}

impl BuiltSubject {
    /// Stable subject identity within one release.
    pub fn identity(&self) -> String {
        format!("{}/{}", self.release_unit, self.identity)
    }
}

/// Release identities every phase tag of one invocation binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseBindings<'a> {
    /// Source commit S.
    pub source_commit: &'a str,
    /// Release commit R.
    pub release_commit: &'a str,
    /// Rendered name of the global release tag.
    pub global_tag: &'a str,
    /// Digest of the sealed release plan.
    pub plan_digest: &'a str,
}

impl PhaseBindings<'_> {
    /// Reject bindings a phase tag could never be verified against.
    fn verify(&self) -> Result<()> {
        for (field, value) in [
            ("source-commit", self.source_commit),
            ("release-commit", self.release_commit),
        ] {
            if !is_git_object(value) {
                return Err(Error::Validation(format!(
                    "phase evidence {field} {value:?} is not a complete Git object identifier"
                )));
            }
        }
        if !is_digest(self.plan_digest) {
            return Err(Error::Validation(format!(
                "phase evidence plan-digest {:?} is not a sha256 digest",
                self.plan_digest
            )));
        }
        if self.global_tag.is_empty() {
            return Err(Error::Validation(
                "phase evidence requires the rendered global release tag name".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Assemble the evidence a before-publication tag seals.
///
/// The intended destinations are what configuration selects rather than what any
/// publisher observed, so this phase claims only what the release is committed
/// to publishing and the subjects that already exist to publish.
pub fn build_before_publication(
    bindings: PhaseBindings<'_>,
    subjects: &[BuiltSubject],
    destinations: &[IntendedDestination],
) -> Result<PhaseTagEvidence> {
    bindings.verify()?;
    let mut intended = destinations.to_vec();
    intended.sort_by_key(IntendedDestination::identity);
    intended.dedup();
    let mut sealed = subjects
        .iter()
        .map(|subject| PhaseSubject {
            release_unit: subject.release_unit.clone(),
            identity: subject.identity.clone(),
            version: subject.version.clone(),
            digest: subject.digest.clone(),
            // An empty provenance list states nothing a consumer can verify, so
            // it is omitted rather than sealed as an affirmative claim.
            provenance: subject
                .provenance
                .clone()
                .filter(|provenance| !provenance.is_empty()),
        })
        .collect::<Vec<_>>();
    sort_subjects(&mut sealed);
    Ok(PhaseTagEvidence {
        schema: PHASE_TAG_EVIDENCE_SCHEMA.to_owned(),
        phase: TagPhase::BeforePublication,
        source_commit: bindings.source_commit.to_owned(),
        release_commit: bindings.release_commit.to_owned(),
        global_tag: bindings.global_tag.to_owned(),
        plan_digest: bindings.plan_digest.to_owned(),
        subjects: sealed,
        intended_destinations: Some(intended),
        publisher_evidence: None,
    })
}

/// Assemble the evidence an after-publication tag seals.
///
/// Every subject this phase can truthfully name is already named by a fragment
/// that published it, so requiring a separate subject input would let the tag
/// claim a subject no publication ever distributed.
pub fn build_after_publication(
    bindings: PhaseBindings<'_>,
    fragments: &[PublisherEvidence],
) -> Result<PhaseTagEvidence> {
    bindings.verify()?;
    let mut sealed = fragments.to_vec();
    sealed.sort_by_key(PublisherEvidence::identity);
    let mut subjects: BTreeMap<(String, String), PhaseSubject> = BTreeMap::new();
    for fragment in &sealed {
        let subject = PhaseSubject {
            release_unit: fragment.release_unit.clone(),
            identity: fragment.subject.identity.clone(),
            version: fragment.subject.version.clone(),
            digest: fragment.subject.digest.clone(),
            provenance: None,
        };
        let key = (subject.release_unit.clone(), subject.identity.clone());
        match subjects.get(&key) {
            Some(existing) if existing != &subject => {
                return Err(Error::Validation(format!(
                    "publisher evidence disagrees about subject {}/{}: {}@{} and {}@{}",
                    subject.release_unit,
                    subject.identity,
                    existing.version,
                    existing.digest,
                    subject.version,
                    subject.digest
                )))
            }
            Some(_) => {}
            None => {
                subjects.insert(key, subject);
            }
        }
    }
    let mut subjects = subjects.into_values().collect::<Vec<_>>();
    sort_subjects(&mut subjects);
    Ok(PhaseTagEvidence {
        schema: PHASE_TAG_EVIDENCE_SCHEMA.to_owned(),
        phase: TagPhase::AfterPublication,
        source_commit: bindings.source_commit.to_owned(),
        release_commit: bindings.release_commit.to_owned(),
        global_tag: bindings.global_tag.to_owned(),
        plan_digest: bindings.plan_digest.to_owned(),
        subjects,
        intended_destinations: None,
        publisher_evidence: Some(sealed),
    })
}

/// Order sealed subjects by their complete identity so equal sets encode equally.
fn sort_subjects(subjects: &mut [PhaseSubject]) {
    subjects.sort_by(|left, right| {
        (
            &left.release_unit,
            &left.identity,
            &left.version,
            &left.digest,
        )
            .cmp(&(
                &right.release_unit,
                &right.identity,
                &right.version,
                &right.digest,
            ))
    });
}

/// Encode phase evidence as the single record line an annotated tag carries.
pub fn encode(evidence: &PhaseTagEvidence) -> Result<String> {
    verify_members(evidence)?;
    let encoded = canonical_json(evidence)?;
    if encoded.contains('\n') || encoded.contains('\r') {
        return Err(Error::Validation(
            "encoded phase evidence contains a line break; a tag record line ends at the message \
             newline"
                .to_owned(),
        ));
    }
    Ok(encoded)
}

/// Decode the phase evidence one annotated tag record line carries.
pub fn decode(value: &str) -> Result<PhaseTagEvidence> {
    let evidence: PhaseTagEvidence = serde_json::from_str(value).map_err(|error| {
        Error::Validation(format!(
            "phase evidence is not a valid record value: {error}"
        ))
    })?;
    if evidence.schema != PHASE_TAG_EVIDENCE_SCHEMA {
        return Err(Error::Validation(format!(
            "phase evidence declares $schema {:?} instead of {PHASE_TAG_EVIDENCE_SCHEMA}",
            evidence.schema
        )));
    }
    verify_members(&evidence)?;
    Ok(evidence)
}

/// Enforce the claim each phase requires and the claim it forbids.
///
/// The published schema pairs one required member with one forbidden member per
/// phase, and the serde layer cannot express that pairing because both members
/// are optional. Without this check a document sealing nothing, or one carrying
/// a claim its phase cannot make, would satisfy every consumer by omission.
fn verify_members(evidence: &PhaseTagEvidence) -> Result<()> {
    let (required, present, forbidden, absent) = match evidence.phase {
        TagPhase::BeforePublication => (
            "intended-destinations",
            evidence.intended_destinations.is_some(),
            "publisher-evidence",
            evidence.publisher_evidence.is_none(),
        ),
        TagPhase::AfterPublication => (
            "publisher-evidence",
            evidence.publisher_evidence.is_some(),
            "intended-destinations",
            evidence.intended_destinations.is_none(),
        ),
    };
    if !present {
        return Err(Error::Validation(format!(
            "phase evidence declares {} without {required}",
            evidence.phase
        )));
    }
    if !absent {
        return Err(Error::Validation(format!(
            "phase evidence declares {} but carries {forbidden}",
            evidence.phase
        )));
    }
    Ok(())
}

/// Read every built-subject document one evidence input directory offers.
///
/// Documents are recognized by schema identity so a build job stays free to name
/// its own files and to place unrelated documents beside them. A document is
/// checked against the workspace it will be sealed for: a subject naming a
/// release unit this configuration does not declare belongs to some other
/// release, and no later comparison would find it, because nothing expects a
/// fragment for a unit that does not exist.
pub fn load_built_subjects(config: &Config, input: &Path) -> Result<Vec<BuiltSubject>> {
    let mut subjects: BTreeMap<String, BuiltSubject> = BTreeMap::new();
    for (path, document) in documents(input)? {
        if document.get("$schema").and_then(serde_yaml::Value::as_str) != Some(BUILT_SUBJECT_SCHEMA)
        {
            continue;
        }
        let subject: BuiltSubject = serde_yaml::from_value(document).map_err(|error| {
            Error::Validation(format!(
                "built subject {} is not schema-valid: {error}",
                path.display()
            ))
        })?;
        if subject.contract != BUILT_SUBJECT_CONTRACT {
            return Err(Error::Validation(format!(
                "built subject {} declares contract {:?} instead of {BUILT_SUBJECT_CONTRACT}",
                path.display(),
                subject.contract
            )));
        }
        if !is_digest(&subject.digest) {
            return Err(Error::Validation(format!(
                "built subject {} records digest {:?}, which is not a sha256 digest",
                path.display(),
                subject.digest
            )));
        }
        if !config.release_units.contains_key(&subject.release_unit) {
            return Err(Error::Validation(format!(
                "built subject {} names release unit {:?}, which this workspace does not declare",
                path.display(),
                subject.release_unit
            )));
        }
        let identity = subject.identity();
        if subjects.insert(identity.clone(), subject).is_some() {
            return Err(Error::Validation(format!(
                "built subject {identity} was supplied more than once"
            )));
        }
    }
    Ok(subjects.into_values().collect())
}

/// Record one built subject from the release the repository itself proves.
///
/// Neither the version nor the digest is supplied by the caller. The version is
/// the one the verified global release tag reproduces for that release unit, so
/// a build job cannot record a subject under some other release's version, and
/// the digest is computed here over the bytes the build actually produced, so a
/// build job cannot claim bytes it did not emit. Everything a later phase tag
/// seals about a subject therefore comes from the repository or from the file
/// system, and only the subject's destination identity is asserted.
pub fn record_built_subject(
    root: &Path,
    release_unit: &str,
    identity: &str,
    subject: &Path,
    output: &Path,
) -> Result<BuiltSubject> {
    if identity.trim().is_empty() {
        return Err(Error::Validation(
            "a built subject records the identity every destination resolves".to_owned(),
        ));
    }
    let verified = verify_release_tag(root)?;
    let version = verified.versions.get(release_unit).ok_or_else(|| {
        Error::Validation(format!(
            "release unit {release_unit:?} publishes nothing in global release tag {}; it releases {}",
            verified.global_tag,
            if verified.versions.is_empty() {
                "nothing".to_owned()
            } else {
                verified
                    .versions
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ))
    })?;
    let built = BuiltSubject {
        schema: BUILT_SUBJECT_SCHEMA.to_owned(),
        contract: BUILT_SUBJECT_CONTRACT.to_owned(),
        release_unit: release_unit.to_owned(),
        identity: identity.to_owned(),
        version: version.clone(),
        digest: digest_subject(subject)?,
        provenance: None,
    };
    if output.exists() {
        return Err(Error::Validation(format!(
            "built-subject output {} already exists; one subject is built once",
            output.display()
        )));
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    }
    let document = serde_yaml::to_string(&built)?;
    std::fs::write(output, document).map_err(|error| Error::io(output, error))?;
    Ok(built)
}

/// Digest the bytes one build produced, whether it produced one file or a tree.
///
/// A directory digests as its ordered member manifest rather than as a concatenation,
/// so a build that renames a member without changing any byte still changes the
/// subject digest. The manifest is the release-unit-relative path and the member
/// digest of every regular file, sorted by path.
fn digest_subject(subject: &Path) -> Result<String> {
    if subject.is_file() {
        return digest_file(subject);
    }
    if !subject.is_dir() {
        return Err(Error::Validation(format!(
            "built subject bytes {} do not exist",
            subject.display()
        )));
    }
    let mut members = walkdir::WalkDir::new(subject)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    members.sort();
    if members.is_empty() {
        return Err(Error::Validation(format!(
            "built subject bytes {} contain no file; a subject with no bytes has no digest",
            subject.display()
        )));
    }
    let mut manifest = String::new();
    for member in &members {
        let relative = member.strip_prefix(subject).unwrap_or(member);
        manifest.push_str(&format!(
            "{}\u{0}{}\n",
            relative.display(),
            digest_file(member)?
        ));
    }
    Ok(digest_bytes(manifest.as_bytes()))
}

/// Read every publisher-evidence fragment one evidence input directory offers.
pub fn load_publisher_evidence(input: &Path) -> Result<Vec<PublisherEvidence>> {
    let mut fragments: BTreeMap<String, PublisherEvidence> = BTreeMap::new();
    for (path, document) in documents(input)? {
        if document.get("$schema").and_then(serde_yaml::Value::as_str)
            != Some(PUBLISHER_EVIDENCE_SCHEMA)
        {
            continue;
        }
        let fragment: PublisherEvidence = serde_yaml::from_value(document).map_err(|error| {
            Error::Validation(format!(
                "publisher evidence {} is not schema-valid: {error}",
                path.display()
            ))
        })?;
        let identity = fragment.identity();
        if fragments.insert(identity.clone(), fragment).is_some() {
            return Err(Error::Validation(format!(
                "publisher evidence {identity} was supplied more than once"
            )));
        }
    }
    Ok(fragments.into_values().collect())
}

/// Every parsable YAML document beneath one evidence input directory, in stable order.
fn documents(input: &Path) -> Result<Vec<(std::path::PathBuf, serde_yaml::Value)>> {
    if !input.is_dir() {
        return Err(Error::Validation(format!(
            "phase evidence input directory {} does not exist",
            input.display()
        )));
    }
    let mut candidates = walkdir::WalkDir::new(input)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|candidate| {
            matches!(
                candidate
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("yml" | "yaml")
            )
        })
        .collect::<Vec<_>>();
    candidates.sort();
    let mut documents = Vec::new();
    for candidate in candidates {
        let text =
            std::fs::read_to_string(&candidate).map_err(|error| Error::io(&candidate, error))?;
        let Ok(document) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        documents.push((candidate, document));
    }
    Ok(documents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::assemble::{
        CleanClient, CleanClientMode, Destination, PackagerRecord, Subject, TagIdentity,
        PUBLISHER_EVIDENCE_CONTRACT,
    };
    use crate::executor::fixture::Workspace;
    use crate::model::PublisherKind;

    const SOURCE: &str = "1111111111111111111111111111111111111111";
    const RELEASE: &str = "2222222222222222222222222222222222222222";
    const PLAN_DIGEST: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";
    const SUBJECT_DIGEST: &str =
        "sha256:5555555555555555555555555555555555555555555555555555555555555555";

    // Version and digest are the two claims a phase tag seals about a subject
    // that a later cross-check compares. Both are derived here rather than
    // supplied, so a build job cannot record a version this release does not
    // publish or bytes it did not produce.
    #[test]
    fn records_a_built_subject_from_the_proved_release_and_the_produced_bytes() {
        let released = crate::release::tag::tests::ReleasedWorkspace::new();
        let root = released.root.clone();
        let bytes = root.join("build/subject.tgz");
        std::fs::create_dir_all(bytes.parent().expect("parent")).expect("build directory");
        std::fs::write(&bytes, b"subject bytes").expect("built bytes");
        let output = root.join("staged/built-subject.yml");

        let built = record_built_subject(&root, "widget", "sample-widget", &bytes, &output)
            .expect("the built subject is recorded");
        assert_eq!(built.schema, BUILT_SUBJECT_SCHEMA);
        assert_eq!(built.identity(), "widget/sample-widget");
        assert_eq!(
            built.version, "1.1.0",
            "the version is the one the verified global release tag reproduces"
        );
        assert_eq!(
            built.digest,
            digest_bytes(b"subject bytes"),
            "the digest is computed over the bytes the build produced"
        );
        let config = Config::load(&root).expect("configuration");
        let staged = load_built_subjects(&config, output.parent().expect("staged"))
            .expect("the written document loads as staged evidence");
        assert_eq!(staged, vec![built]);

        // One subject is built once, so a second recording into the same
        // document is a collision rather than a repeat.
        let error = record_built_subject(&root, "widget", "sample-widget", &bytes, &output)
            .expect_err("a second recording into the same output is refused");
        assert!(
            error.to_string().contains("one subject is built once"),
            "{error}"
        );
    }

    // A release unit this release does not publish has no version to record, so
    // recording one under some other unit's version would bind the subject to a
    // release it is not part of.
    #[test]
    fn refuses_a_subject_for_a_release_unit_the_release_does_not_publish() {
        let released = crate::release::tag::tests::ReleasedWorkspace::new();
        let root = released.root.clone();
        let bytes = root.join("build/subject.tgz");
        std::fs::create_dir_all(bytes.parent().expect("parent")).expect("build directory");
        std::fs::write(&bytes, b"subject bytes").expect("built bytes");

        let error = record_built_subject(
            &root,
            "absent-unit",
            "sample-widget",
            &bytes,
            &root.join("staged/absent.yml"),
        )
        .expect_err("a release unit outside this release is refused");
        assert!(
            error
                .to_string()
                .contains("publishes nothing in global release tag"),
            "{error}"
        );
        assert!(
            !root.join("staged/absent.yml").exists(),
            "a refused recording writes nothing"
        );
    }

    // A build that produces a tree digests as its ordered member manifest, so
    // moving a member without changing a byte still changes the subject. A
    // digest over the concatenated members alone would call those two builds
    // the same subject.
    #[test]
    fn a_renamed_member_changes_the_digest_of_a_built_tree() {
        let released = crate::release::tag::tests::ReleasedWorkspace::new();
        let root = released.root.clone();
        let tree = root.join("build/tree");
        std::fs::create_dir_all(&tree).expect("build tree");
        std::fs::write(tree.join("first"), b"one").expect("member");
        std::fs::write(tree.join("second"), b"two").expect("member");
        let first =
            record_built_subject(&root, "widget", "sample-widget", &tree, &root.join("a.yml"))
                .expect("the built tree is recorded");

        std::fs::rename(tree.join("second"), tree.join("third")).expect("rename a member");
        let renamed =
            record_built_subject(&root, "widget", "sample-widget", &tree, &root.join("b.yml"))
                .expect("the renamed tree is recorded");
        assert_ne!(
            first.digest, renamed.digest,
            "a renamed member is a different subject"
        );
    }

    fn bindings() -> PhaseBindings<'static> {
        PhaseBindings {
            source_commit: SOURCE,
            release_commit: RELEASE,
            global_tag: "release/1.0.0",
            plan_digest: PLAN_DIGEST,
        }
    }

    fn built(identity: &str) -> BuiltSubject {
        BuiltSubject {
            schema: BUILT_SUBJECT_SCHEMA.to_owned(),
            contract: BUILT_SUBJECT_CONTRACT.to_owned(),
            release_unit: "component".to_owned(),
            identity: identity.to_owned(),
            version: "1.0.0".to_owned(),
            digest: SUBJECT_DIGEST.to_owned(),
            provenance: None,
        }
    }

    fn destination(publisher: PublisherKind, target: &str) -> IntendedDestination {
        IntendedDestination {
            release_unit: "component".to_owned(),
            package: "package".to_owned(),
            publisher,
            target: target.to_owned(),
        }
    }

    fn fragment(target: &str, identity: &str) -> PublisherEvidence {
        PublisherEvidence {
            schema: PUBLISHER_EVIDENCE_SCHEMA.to_owned(),
            contract: PUBLISHER_EVIDENCE_CONTRACT.to_owned(),
            release_unit: "component".to_owned(),
            package: "package".to_owned(),
            publisher: PublisherKind::Npm,
            target: target.to_owned(),
            source_commit: SOURCE.to_owned(),
            release_commit: RELEASE.to_owned(),
            global_tag: TagIdentity {
                name: "release/1.0.0".to_owned(),
                object: SOURCE.to_owned(),
                target: RELEASE.to_owned(),
            },
            plan_digest: PLAN_DIGEST.to_owned(),
            subject: Subject {
                kind: "npm-package".to_owned(),
                identity: identity.to_owned(),
                version: "1.0.0".to_owned(),
                digest: SUBJECT_DIGEST.to_owned(),
            },
            packager: PackagerRecord {
                id: "npm".to_owned(),
                version: "10.8.2".to_owned(),
            },
            build_provenance: Vec::new(),
            attached_metadata: Vec::new(),
            destination: Destination {
                identity: "registry.example.test/sample-library".to_owned(),
                version: "1.0.0".to_owned(),
                digest: "sha512-example".to_owned(),
            },
            clean_client: CleanClient {
                mode: CleanClientMode::Public,
                client: "npm".to_owned(),
                version: "10.8.2".to_owned(),
                digest: "sha512-example".to_owned(),
            },
            destination_aliases: Vec::new(),
            phase_tags: Vec::new(),
        }
    }

    #[test]
    fn encodes_and_decodes_one_single_line_record_value() {
        let evidence = build_before_publication(
            bindings(),
            &[built("sample-library")],
            &[destination(PublisherKind::Npm, "primary")],
        )
        .expect("before-publication evidence");
        let encoded = encode(&evidence).expect("encoded evidence");
        assert!(!encoded.contains('\n'), "{encoded}");
        assert_eq!(decode(&encoded).expect("decoded evidence"), evidence);
    }

    #[test]
    fn a_record_value_survives_the_record_separator_inside_its_strings() {
        let evidence = build_before_publication(
            bindings(),
            &[built("sample-library: staged")],
            &[destination(PublisherKind::Npm, "primary: staged")],
        )
        .expect("before-publication evidence");
        let encoded = encode(&evidence).expect("encoded evidence");
        let line = format!("{PHASE_EVIDENCE_FIELD}: {encoded}");
        let (field, value) = line.split_once(": ").expect("record separator");
        assert_eq!(field, PHASE_EVIDENCE_FIELD);
        assert_eq!(decode(value).expect("decoded evidence"), evidence);
    }

    #[test]
    fn equal_evidence_built_in_different_orders_encodes_identically() {
        let first = build_before_publication(
            bindings(),
            &[built("sample-library"), built("sample-tool")],
            &[
                destination(PublisherKind::Npm, "primary"),
                destination(PublisherKind::Npm, "github"),
            ],
        )
        .expect("first evidence");
        let second = build_before_publication(
            bindings(),
            &[built("sample-tool"), built("sample-library")],
            &[
                destination(PublisherKind::Npm, "github"),
                destination(PublisherKind::Npm, "primary"),
            ],
        )
        .expect("second evidence");
        assert_eq!(
            encode(&first).expect("first encoding"),
            encode(&second).expect("second encoding")
        );
    }

    #[test]
    fn rejects_a_record_value_declaring_a_foreign_schema() {
        let evidence =
            build_after_publication(bindings(), &[fragment("primary", "sample-library")])
                .expect("after-publication evidence");
        let encoded = encode(&evidence).expect("encoded evidence").replace(
            PHASE_TAG_EVIDENCE_SCHEMA,
            "https://example.test/schemas/other/v1",
        );
        let error = decode(&encoded).expect_err("a foreign schema is rejected");
        assert!(error.to_string().contains("instead of"), "{error}");
    }

    #[test]
    fn rejects_a_phase_carrying_the_member_it_forbids() {
        let mut evidence =
            build_after_publication(bindings(), &[fragment("primary", "sample-library")])
                .expect("after-publication evidence");
        evidence.intended_destinations = Some(vec![destination(PublisherKind::Npm, "primary")]);
        let error = encode(&evidence).expect_err("a forbidden member is rejected");
        assert!(
            error
                .to_string()
                .contains("after-publication but carries intended-destinations"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_phase_missing_the_member_it_requires() {
        let mut evidence = build_before_publication(
            bindings(),
            &[built("sample-library")],
            &[destination(PublisherKind::Npm, "primary")],
        )
        .expect("before-publication evidence");
        evidence.intended_destinations = None;
        let error = encode(&evidence).expect_err("a missing member is rejected");
        assert!(
            error
                .to_string()
                .contains("before-publication without intended-destinations"),
            "{error}"
        );
    }

    #[test]
    fn seals_the_accepted_fragments_and_derives_their_subjects() {
        let evidence = build_after_publication(
            bindings(),
            &[
                fragment("primary", "sample-library"),
                fragment("github", "sample-library"),
            ],
        )
        .expect("after-publication evidence");
        assert_eq!(
            evidence
                .publisher_evidence
                .as_ref()
                .expect("sealed fragments")
                .iter()
                .map(PublisherEvidence::identity)
                .collect::<Vec<_>>(),
            vec![
                "component/package/npm/github",
                "component/package/npm/primary"
            ]
        );
        assert_eq!(evidence.subjects.len(), 1, "one subject, two destinations");
        assert_eq!(evidence.subjects[0].digest, SUBJECT_DIGEST);
    }

    #[test]
    fn rejects_fragments_that_disagree_about_one_subject() {
        let mut conflicting = fragment("github", "sample-library");
        conflicting.subject.version = "1.0.1".to_owned();
        let error = build_after_publication(
            bindings(),
            &[fragment("primary", "sample-library"), conflicting],
        )
        .expect_err("disagreement is rejected");
        assert!(
            error.to_string().contains("disagrees about subject"),
            "{error}"
        );
    }

    /// A workspace declaring the one release unit these documents name.
    fn configured(label: &str) -> (Workspace, Config) {
        let workspace = Workspace::new(label);
        workspace.write(
            ".intentional/config.yml",
            "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-2\nrelease-units:\n  component:\n    path: component\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n",
        );
        let config = Config::load(workspace.root()).expect("configuration");
        (workspace, config)
    }

    #[test]
    fn loads_built_subjects_by_document_identity() {
        let (workspace, config) = configured("phase-built-subjects");
        workspace
            .write(
                "evidence/library.yml",
                &format!(
                    "$schema: {BUILT_SUBJECT_SCHEMA}\ncontract: {BUILT_SUBJECT_CONTRACT}\nrelease-unit: component\nidentity: sample-library\nversion: 1.0.0\ndigest: {SUBJECT_DIGEST}\n"
                ),
            )
            .write("evidence/unrelated.yml", "$schema: https://example.test/schemas/other/v1\nvalue: ignored\n")
            .write("evidence/notes.txt", "not a document\n");
        let subjects = load_built_subjects(&config, &workspace.root().join("evidence"))
            .expect("built subjects");
        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0].identity, "sample-library");
    }

    #[test]
    fn rejects_two_built_subjects_for_one_release_unit_identity() {
        let (workspace, config) = configured("phase-built-duplicate");
        let document = format!(
            "$schema: {BUILT_SUBJECT_SCHEMA}\ncontract: {BUILT_SUBJECT_CONTRACT}\nrelease-unit: component\nidentity: sample-library\nversion: 1.0.0\ndigest: {SUBJECT_DIGEST}\n"
        );
        workspace
            .write("evidence/first.yml", &document)
            .write("evidence/second.yml", &document);
        let error = load_built_subjects(&config, &workspace.root().join("evidence"))
            .expect_err("a duplicate subject is rejected");
        assert!(
            error
                .to_string()
                .contains("component/sample-library was supplied more than once"),
            "{error}"
        );
    }
    #[test]
    fn rejects_a_built_subject_naming_an_undeclared_release_unit() {
        let (workspace, config) = configured("phase-built-foreign-unit");
        workspace.write(
            "evidence/foreign.yml",
            &format!(
                "$schema: {BUILT_SUBJECT_SCHEMA}\ncontract: {BUILT_SUBJECT_CONTRACT}\nrelease-unit: other-component\nidentity: sample-library\nversion: 1.0.0\ndigest: {SUBJECT_DIGEST}\n"
            ),
        );
        let error = load_built_subjects(&config, &workspace.root().join("evidence"))
            .expect_err("a subject from another workspace is rejected");
        assert!(
            error
                .to_string()
                .contains("which this workspace does not declare"),
            "{error}"
        );
    }
}
