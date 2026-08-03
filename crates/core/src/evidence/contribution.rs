// ---
// relationships:
//   implements: github-release-executor
// ---

//! Repository-owned evidence contributions and their workflow transport.

use crate::error::{Error, Result};
use crate::evidence::{
    copy_and_digest, digest_bytes, is_digest, is_flat_name, is_namespace, prepare_output,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Schema identity every contribution manifest carries.
pub const CONTRIBUTION_SCHEMA: &str = "https://intentional.foo/schemas/contribution/v1";
/// Manifest file name inside a contribution bundle.
pub const CONTRIBUTION_MANIFEST: &str = "contribution.yml";
/// Bundle subdirectory holding contributed attachment files.
pub const ATTACHMENTS_DIRECTORY: &str = "attachments";
/// Protocol prefix of every contribution workflow artifact name.
pub const CONTRIBUTION_ARTIFACT_PREFIX: &str = "intentional-contribution";
/// Job identity used when a contribution is constructed outside GitHub Actions.
pub const LOCAL_JOB: &str = "local";

const NAMESPACE_HASH_LENGTH: usize = 16;

/// One contributed file inventoried by its Release asset name, bundle-relative
/// path, and digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionAttachment {
    /// Flat GitHub Release asset name.
    pub name: String,
    /// Bundle-relative path of the transported file.
    pub file: String,
    /// Digest of the transported bytes.
    pub sha256: String,
}

/// Transport manifest describing one contributor namespace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionManifest {
    /// Contribution schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Namespace that owns the contributed value and attachments.
    pub namespace: String,
    /// Arbitrary contributor YAML, transported without interpretation.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<serde_yaml::Value>,
    /// Inventoried contributed files.
    pub attachments: Vec<ContributionAttachment>,
}

/// Deserialize a present `value` field, including an explicit YAML null.
///
/// The default `Option` deserializer cannot distinguish an absent value from a
/// contributed null, and a contributed null is a legal contribution.
fn present_value<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<serde_yaml::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_yaml::Value::deserialize(deserializer).map(Some)
}

impl ContributionManifest {
    /// Report every mechanical problem in this manifest.
    pub fn findings(&self, label: &str) -> Vec<String> {
        let mut findings = Vec::new();
        if self.schema != CONTRIBUTION_SCHEMA {
            findings.push(format!(
                "{label} declares $schema {} instead of {CONTRIBUTION_SCHEMA}",
                self.schema
            ));
        }
        if !is_namespace(&self.namespace) {
            findings.push(format!(
                "{label} declares an unusable contributor namespace {:?}",
                self.namespace
            ));
        }
        if self.value.is_none() && self.attachments.is_empty() {
            findings.push(format!(
                "{label} contains neither a value nor an attachment"
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        for attachment in &self.attachments {
            if !is_flat_name(&attachment.name) {
                findings.push(format!(
                    "{label} inventories an unusable Release asset name {:?}",
                    attachment.name
                ));
                continue;
            }
            if !names.insert(attachment.name.as_str()) {
                findings.push(format!(
                    "{label} inventories Release asset {} more than once",
                    attachment.name
                ));
            }
            let expected = format!("{ATTACHMENTS_DIRECTORY}/{}", attachment.name);
            if attachment.file != expected {
                findings.push(format!(
                    "{label} inventories {} at {} instead of {expected}",
                    attachment.name, attachment.file
                ));
            }
            if !is_digest(&attachment.sha256) {
                findings.push(format!(
                    "{label} inventories {} with an unusable digest {}",
                    attachment.name, attachment.sha256
                ));
            }
        }
        findings
    }
}

/// Inputs of one `intentional evidence contribute` invocation.
#[derive(Debug, Clone)]
pub struct ContributionRequest<'a> {
    /// Namespace assigned to the contribution.
    pub namespace: &'a str,
    /// File containing any valid YAML value.
    pub value_file: Option<&'a Path>,
    /// Exact file paths to transport as Release attachments.
    pub attachments: &'a [PathBuf],
    /// Directory the bundle is written into.
    pub output: &'a Path,
    /// GitHub job identifier that owns this contribution.
    pub job: &'a str,
    /// Workflow run attempt that produced this contribution.
    pub run_attempt: u32,
}

/// One constructed contribution bundle and the artifact name that transports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContributionBundle {
    /// Directory containing the manifest and attachment files.
    pub path: PathBuf,
    /// Written manifest path.
    pub manifest_path: PathBuf,
    /// Collision-safe temporary workflow artifact name.
    pub artifact_name: String,
    /// Manifest written into the bundle.
    pub manifest: ContributionManifest,
}

/// Construct one contribution bundle from contributor-supplied content.
pub fn contribute(request: &ContributionRequest<'_>) -> Result<ContributionBundle> {
    let mut findings = Vec::new();
    if !is_namespace(request.namespace) {
        findings.push(format!(
            "contribution namespace {:?} must be non-empty, untrimmed, and free of control characters",
            request.namespace
        ));
    }
    if request.value_file.is_none() && request.attachments.is_empty() {
        findings.push("at least one value file or attachment is required".to_owned());
    }
    let mut sources: Vec<(String, &Path)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for attachment in request.attachments {
        let Some(name) = attachment.file_name().and_then(|name| name.to_str()) else {
            findings.push(format!(
                "attachments must be exact file paths; {} has no usable file name",
                attachment.display()
            ));
            continue;
        };
        if !attachment.metadata().is_ok_and(|meta| meta.is_file()) {
            findings.push(format!(
                "attachments must be exact file paths; {} is not an existing regular file",
                attachment.display()
            ));
            continue;
        }
        if !is_flat_name(name) {
            findings.push(format!(
                "attachment {} has an unusable Release asset name {name:?}",
                attachment.display()
            ));
            continue;
        }
        if !seen.insert(name.to_owned()) {
            findings.push(format!(
                "Release asset name {name} is contributed more than once by this contribution"
            ));
            continue;
        }
        sources.push((name.to_owned(), attachment.as_path()));
    }
    let value = match request.value_file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => match serde_yaml::from_str::<serde_yaml::Value>(&text) {
                Ok(value) => Some(value),
                Err(error) => {
                    findings.push(format!(
                        "value file {} is not one valid YAML value: {error}",
                        path.display()
                    ));
                    None
                }
            },
            Err(error) => {
                findings.push(format!(
                    "value file {} is unreadable: {error}",
                    path.display()
                ));
                None
            }
        },
        None => None,
    };
    if !findings.is_empty() {
        return Err(Error::Validation(findings.join("\n")));
    }

    prepare_output(request.output, "contribution")?;
    let attachments_directory = request.output.join(ATTACHMENTS_DIRECTORY);
    if !sources.is_empty() {
        std::fs::create_dir_all(&attachments_directory)
            .map_err(|error| Error::io(&attachments_directory, error))?;
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    let mut attachments = Vec::new();
    for (name, source) in &sources {
        let destination = attachments_directory.join(name);
        let sha256 = copy_and_digest(source, &destination)?;
        attachments.push(ContributionAttachment {
            name: name.clone(),
            file: format!("{ATTACHMENTS_DIRECTORY}/{name}"),
            sha256,
        });
    }
    let manifest = ContributionManifest {
        schema: CONTRIBUTION_SCHEMA.to_owned(),
        namespace: request.namespace.to_owned(),
        value,
        attachments,
    };
    let manifest_path = request.output.join(CONTRIBUTION_MANIFEST);
    std::fs::write(&manifest_path, serde_yaml::to_string(&manifest)?)
        .map_err(|error| Error::io(&manifest_path, error))?;
    Ok(ContributionBundle {
        path: request.output.to_path_buf(),
        manifest_path,
        artifact_name: artifact_name(request.namespace, request.job, request.run_attempt),
        manifest,
    })
}

/// Namespace hash used by contribution artifact names.
///
/// The hash keeps arbitrary contributor namespaces out of GitHub artifact
/// names while still giving assembly a stable grouping key it can bind back to
/// the namespace the manifest declares.
pub fn namespace_hash(namespace: &str) -> String {
    digest_bytes(namespace.as_bytes())
        .trim_start_matches("sha256:")
        .chars()
        .take(NAMESPACE_HASH_LENGTH)
        .collect()
}

/// Collision-safe artifact name transporting one contribution bundle.
pub fn artifact_name(namespace: &str, job: &str, run_attempt: u32) -> String {
    format!(
        "{CONTRIBUTION_ARTIFACT_PREFIX}-{}-{}-{run_attempt}",
        namespace_hash(namespace),
        job_slug(job)
    )
}

/// Reduce a job identifier to the characters a GitHub artifact name accepts.
fn job_slug(job: &str) -> String {
    let slug: String = job
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if slug.is_empty() {
        LOCAL_JOB.to_owned()
    } else {
        slug
    }
}

/// Contributor identity recovered from one contribution artifact name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContributionArtifactName {
    /// Namespace hash claimed by the artifact.
    pub namespace_hash: String,
    /// Job that produced the contribution.
    pub job: String,
    /// Workflow run attempt that produced the contribution.
    pub run_attempt: u32,
}

/// Recover the contributor identity a contribution artifact name encodes.
pub fn parse_artifact_name(name: &str) -> Option<ContributionArtifactName> {
    let remainder = name
        .strip_prefix(CONTRIBUTION_ARTIFACT_PREFIX)?
        .strip_prefix('-')?;
    let (namespace_hash, remainder) = remainder.split_at_checked(NAMESPACE_HASH_LENGTH)?;
    if !namespace_hash
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let (job, run_attempt) = remainder.strip_prefix('-')?.rsplit_once('-')?;
    if job.is_empty() {
        return None;
    }
    Some(ContributionArtifactName {
        namespace_hash: namespace_hash.to_owned(),
        job: job.to_owned(),
        run_attempt: run_attempt.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;

    fn request<'a>(
        workspace: &'a Workspace,
        namespace: &'a str,
        value_file: Option<&'a Path>,
        attachments: &'a [PathBuf],
        output: &'a Path,
    ) -> ContributionRequest<'a> {
        let _ = workspace;
        ContributionRequest {
            namespace,
            value_file,
            attachments,
            output,
            job: "assess",
            run_attempt: 1,
        }
    }

    #[test]
    fn stages_a_value_and_an_attachment() {
        let workspace = Workspace::new("contribute-value");
        workspace
            .write("value.yml", "findings: 0\nstatus: clean\n")
            .write("output/report.json", "{\"ok\":true}");
        let attachments = vec![workspace.root().join("output/report.json")];
        let value_file = workspace.root().join("value.yml");
        let output = workspace.root().join("bundle");
        let bundle = contribute(&request(
            &workspace,
            "assessment",
            Some(&value_file),
            &attachments,
            &output,
        ))
        .expect("contribution constructed");
        assert_eq!(bundle.manifest.attachments.len(), 1);
        assert_eq!(bundle.manifest.attachments[0].name, "report.json");
        assert_eq!(
            bundle.manifest.attachments[0].file,
            "attachments/report.json"
        );
        assert_eq!(
            bundle.manifest.attachments[0].sha256,
            digest_bytes(b"{\"ok\":true}")
        );
        assert!(output.join("attachments/report.json").is_file());
        assert_eq!(
            bundle.artifact_name,
            format!(
                "{CONTRIBUTION_ARTIFACT_PREFIX}-{}-assess-1",
                namespace_hash("assessment")
            )
        );
    }

    #[test]
    fn transports_every_yaml_value_kind_without_interpretation() {
        for (label, text, expected) in [
            ("scalar", "17\n", serde_yaml::Value::from(17)),
            (
                "sequence",
                "- one\n- two\n",
                serde_yaml::from_str("- one\n- two\n").expect("sequence"),
            ),
            (
                "mapping",
                "outcome: pass\n",
                serde_yaml::from_str("outcome: pass\n").expect("mapping"),
            ),
            ("null", "null\n", serde_yaml::Value::Null),
            ("empty", "", serde_yaml::Value::Null),
            (
                "string",
                "\"free text\"\n",
                serde_yaml::Value::from("free text"),
            ),
        ] {
            let workspace = Workspace::new(&format!("contribute-{label}"));
            workspace.write("value.yml", text);
            let value_file = workspace.root().join("value.yml");
            let output = workspace.root().join("bundle");
            let bundle = contribute(&request(
                &workspace,
                "observations",
                Some(&value_file),
                &[],
                &output,
            ))
            .expect("contribution constructed");
            assert_eq!(bundle.manifest.value, Some(expected), "{label}");
            let written = std::fs::read_to_string(&bundle.manifest_path).expect("manifest");
            let reparsed: ContributionManifest =
                serde_yaml::from_str(&written).expect("manifest round-trips");
            assert_eq!(reparsed, bundle.manifest, "{label}");
            assert!(reparsed.findings("manifest").is_empty(), "{label}");
        }
    }

    #[test]
    fn requires_contribution_content() {
        let workspace = Workspace::new("contribute-empty");
        let output = workspace.root().join("bundle");
        let error = contribute(&request(&workspace, "empty", None, &[], &output))
            .expect_err("empty contribution rejected");
        assert!(
            error
                .to_string()
                .contains("at least one value file or attachment is required"),
            "{error}"
        );
        assert!(!output.exists(), "a rejected contribution writes nothing");
    }

    #[test]
    fn rejects_attachments_that_are_not_exact_files() {
        let workspace = Workspace::new("contribute-glob");
        workspace.write("output/report.json", "{}");
        let attachments = vec![
            workspace.root().join("output/*"),
            workspace.root().join("output"),
            workspace.root().join("output/missing.json"),
        ];
        let output = workspace.root().join("bundle");
        let error = contribute(&request(
            &workspace,
            "assessment",
            None,
            &attachments,
            &output,
        ))
        .expect_err("inexact attachments rejected");
        assert_eq!(
            error
                .to_string()
                .matches("attachments must be exact file paths")
                .count(),
            3,
            "one run reports every inexact attachment: {error}"
        );
        assert!(!output.exists(), "a rejected contribution writes nothing");
    }

    #[test]
    fn rejects_repeated_release_asset_names() {
        let workspace = Workspace::new("contribute-duplicate");
        workspace
            .write("first/report.json", "{}")
            .write("second/report.json", "{}");
        let attachments = vec![
            workspace.root().join("first/report.json"),
            workspace.root().join("second/report.json"),
        ];
        let output = workspace.root().join("bundle");
        let error = contribute(&request(
            &workspace,
            "assessment",
            None,
            &attachments,
            &output,
        ))
        .expect_err("repeated asset name rejected");
        assert!(
            error.to_string().contains("contributed more than once"),
            "{error}"
        );
    }

    #[test]
    fn refuses_to_write_into_a_populated_bundle_directory() {
        let workspace = Workspace::new("contribute-populated");
        workspace
            .write("bundle/leftover.txt", "stale")
            .write("value.yml", "1\n");
        let value_file = workspace.root().join("value.yml");
        let output = workspace.root().join("bundle");
        let error = contribute(&request(
            &workspace,
            "assessment",
            Some(&value_file),
            &[],
            &output,
        ))
        .expect_err("populated output rejected");
        assert!(error.to_string().contains("is not empty"), "{error}");
    }

    #[test]
    fn artifact_names_round_trip_and_separate_jobs_and_attempts() {
        let name = artifact_name("assessment", "scan-and-report", 12);
        let parsed = parse_artifact_name(&name).expect("artifact name parses");
        assert_eq!(parsed.namespace_hash, namespace_hash("assessment"));
        assert_eq!(parsed.job, "scan_and_report");
        assert_eq!(parsed.run_attempt, 12);
        assert_ne!(
            artifact_name("assessment", "one", 1),
            artifact_name("assessment", "two", 1)
        );
        assert_ne!(
            artifact_name("assessment", "one", 1),
            artifact_name("observations", "one", 1)
        );
        assert_eq!(parse_artifact_name("unrelated-artifact"), None);
        assert_eq!(
            parse_artifact_name(&format!("{CONTRIBUTION_ARTIFACT_PREFIX}-notahash-job-1")),
            None
        );
        assert_eq!(
            parse_artifact_name(&format!(
                "{CONTRIBUTION_ARTIFACT_PREFIX}-{}-job-none",
                namespace_hash("assessment")
            )),
            None
        );
    }
}
