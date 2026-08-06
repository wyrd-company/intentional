// ---
// relationships:
//   implements: github-release-executor
// ---

//! Derive credential populations from emitted workflow bodies for usage witnesses.
//!
//! These helpers read what derivation actually wrote rather than parallel rosters
//! in source. A mutation at the emission seam therefore reaches the witness.

#[cfg(any(test, feature = "test-support"))]
use crate::config::WorkflowRole;
#[cfg(any(test, feature = "test-support"))]
use crate::executor::recipe::StoredCredentialKind;
#[cfg(any(test, feature = "test-support"))]
use serde_yaml::Value;
#[cfg(any(test, feature = "test-support"))]
use std::collections::BTreeSet;

#[cfg(any(test, feature = "test-support"))]
const APP_TOKEN_ACTION_PREFIX: &str = "actions/create-github-app-token@";
#[cfg(any(test, feature = "test-support"))]
const GITHUB_TOKEN_SECRET: &str = "secrets.GITHUB_TOKEN";
#[cfg(any(test, feature = "test-support"))]
const TRUSTED_PUBLISHING_TOKENS: &str = "trusted_publishing/tokens";
#[cfg(any(test, feature = "test-support"))]
const NPM_TRUSTED_PREP_STEP: &str = "Prepare the npm client for trusted publishing";
#[cfg(any(test, feature = "test-support"))]
const LABEL_DOCKER_HUB: &str = "Docker Hub";
#[cfg(any(test, feature = "test-support"))]
const LABEL_AUR: &str = "the AUR";
#[cfg(any(test, feature = "test-support"))]
const LABEL_CARGO_ALTERNATE: &str = "a non-crates.io Cargo registry";
#[cfg(any(test, feature = "test-support"))]
const LABEL_GITHUB_PACKAGES: &str = "GitHub Package Registry";
#[cfg(any(test, feature = "test-support"))]
const NPM_PKG_GITHUB_REGISTRY: &str = "npm.pkg.github.com";
#[cfg(any(test, feature = "test-support"))]
const STANDING_LABEL_ORDER: [&str; 3] = [LABEL_DOCKER_HUB, LABEL_AUR, LABEL_CARGO_ALTERNATE];

#[cfg(any(test, feature = "test-support"))]
fn github_reference_prefix(kind: &str) -> String {
    let mut prefix = String::from('$');
    prefix.push('{');
    prefix.push('{');
    prefix.push(' ');
    prefix.push_str(kind);
    prefix.push('.');
    prefix
}

#[cfg(any(test, feature = "test-support"))]
fn repository_reference(value: &str) -> Option<(StoredCredentialKind, String)> {
    let trimmed = value.trim();
    let suffix = " }}";
    if let Some(name) = trimmed
        .strip_prefix(&github_reference_prefix("vars"))
        .and_then(|tail| tail.strip_suffix(suffix))
    {
        return Some((StoredCredentialKind::RepositoryVariable, name.to_owned()));
    }
    if let Some(name) = trimmed
        .strip_prefix(&github_reference_prefix("secrets"))
        .and_then(|tail| tail.strip_suffix(suffix))
    {
        return Some((StoredCredentialKind::RepositorySecret, name.to_owned()));
    }
    None
}

#[cfg(any(test, feature = "test-support"))]
fn walk_values(value: &Value, visit: &mut impl FnMut(&Value)) {
    match value {
        Value::Mapping(mapping) => {
            for child in mapping.values() {
                walk_values(child, visit);
            }
        }
        Value::Sequence(sequence) => {
            for child in sequence {
                walk_values(child, visit);
            }
        }
        other => visit(other),
    }
}

#[cfg(any(test, feature = "test-support"))]
fn emission_text(body: &Value) -> String {
    let mut collected = String::new();
    walk_values(body, &mut |value| {
        if let Some(text) = value.as_str() {
            collected.push_str(text);
            collected.push('\n');
        }
    });
    collected
}

#[cfg(any(test, feature = "test-support"))]
fn is_app_token_step(step: &Value) -> bool {
    step.get("uses")
        .and_then(Value::as_str)
        .is_some_and(|uses| uses.starts_with(APP_TOKEN_ACTION_PREFIX))
}

#[cfg(any(test, feature = "test-support"))]
fn trusted_publishing_bootstrap_emission(text: &str) -> bool {
    text.contains(TRUSTED_PUBLISHING_TOKENS) || text.contains(NPM_TRUSTED_PREP_STEP)
}

#[cfg(any(test, feature = "test-support"))]
fn standing_label_for_publish_job(job_id: &str, text: &str) -> Option<String> {
    if trusted_publishing_bootstrap_emission(text) {
        return None;
    }
    if job_id.ends_with("_homebrew_primary")
        || job_id.ends_with("_rpm_primary")
        || job_id.ends_with("_apt_primary")
    {
        return None;
    }
    if job_id.ends_with("_oci_ghcr")
        || (job_id.ends_with("_npm_github") && text.contains(GITHUB_TOKEN_SECRET))
        || job_id.ends_with("_npm_primary")
    {
        return None;
    }
    if job_id.ends_with("_oci_dockerhub") || text.contains("DOCKERHUB_TOKEN") {
        return Some(LABEL_DOCKER_HUB.to_owned());
    }
    if job_id.ends_with("_aur_primary") || text.contains("INTENTIONAL_AUR_KEY") {
        return Some(LABEL_AUR.to_owned());
    }
    if job_id.ends_with("_cargo_primary")
        && text.contains("CARGO_REGISTRY_TOKEN")
        && !text.contains(TRUSTED_PUBLISHING_TOKENS)
    {
        return Some(LABEL_CARGO_ALTERNATE.to_owned());
    }
    if job_id.ends_with("_npm_github")
        && text.contains(NPM_PKG_GITHUB_REGISTRY)
        && text.contains(&github_reference_prefix("secrets"))
        && !text.contains(GITHUB_TOKEN_SECRET)
    {
        return Some(LABEL_GITHUB_PACKAGES.to_owned());
    }
    None
}

#[cfg(any(test, feature = "test-support"))]
fn order_standing_labels(found: BTreeSet<String>) -> Vec<String> {
    let mut ordered = Vec::new();
    for label in STANDING_LABEL_ORDER {
        if found.contains(label) {
            ordered.push(label.to_owned());
        }
    }
    for label in found {
        if !STANDING_LABEL_ORDER.contains(&label.as_str()) {
            ordered.push(label);
        }
    }
    ordered
}

/// Stored repository variables and secrets read on GitHub App token mint steps.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn long_lived_repository_write_credentials(
    workflows: &[(WorkflowRole, String)],
) -> Vec<(StoredCredentialKind, String)> {
    let mut variables = BTreeSet::new();
    let mut secrets = BTreeSet::new();

    for (_, workflow) in workflows {
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let jobs = document["jobs"]
            .as_mapping()
            .expect("derived workflow jobs");
        for body in jobs.values() {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            for step in steps {
                if !is_app_token_step(step) {
                    continue;
                }
                let Some(with) = step.get("with").and_then(Value::as_mapping) else {
                    continue;
                };
                for value in with.values() {
                    if let Some((kind, name)) = value.as_str().and_then(repository_reference) {
                        match kind {
                            StoredCredentialKind::RepositoryVariable => {
                                variables.insert(name);
                            }
                            StoredCredentialKind::RepositorySecret => {
                                secrets.insert(name);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut credentials = variables
        .into_iter()
        .map(|name| (StoredCredentialKind::RepositoryVariable, name))
        .collect::<Vec<_>>();
    credentials.extend(
        secrets
            .into_iter()
            .map(|name| (StoredCredentialKind::RepositorySecret, name)),
    );
    credentials
}

/// Reader-facing labels for routes whose emitted bodies authenticate every publication
/// with a repository-stored credential.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn standing_credential_usage_labels(workflows: &[(WorkflowRole, String)]) -> Vec<String> {
    let publish = workflows
        .iter()
        .find(|(role, _)| *role == WorkflowRole::Publish)
        .expect("exhaustive fixture derives publish workflow");
    let document: Value =
        serde_yaml::from_str(&publish.1).expect("derived publish workflow YAML parses");
    let jobs = document["jobs"]
        .as_mapping()
        .expect("derived publish workflow has jobs");
    let mut labels = BTreeSet::new();
    for (job_id, body) in jobs {
        let job_id = job_id.as_str().expect("publish job id");
        if !job_id.starts_with("intentional_publish_") {
            continue;
        }
        if let Some(label) = standing_label_for_publish_job(job_id, &emission_text(body)) {
            labels.insert(label);
        }
    }
    order_standing_labels(labels)
}

#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn trusted_publishing_bootstrap_route_count(workflows: &[(WorkflowRole, String)]) -> usize {
    let publish = workflows
        .iter()
        .find(|(role, _)| *role == WorkflowRole::Publish)
        .expect("exhaustive fixture derives publish workflow");
    let document: Value =
        serde_yaml::from_str(&publish.1).expect("derived publish workflow YAML parses");
    let jobs = document["jobs"]
        .as_mapping()
        .expect("derived publish workflow has jobs");
    jobs.iter()
        .filter(|(job_id, body)| {
            job_id
                .as_str()
                .is_some_and(|id| id.ends_with("_npm_primary") || id.ends_with("_cargo_primary"))
                && trusted_publishing_bootstrap_emission(&emission_text(body))
        })
        .count()
}
