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
const LABEL_HOMEBREW_TAP: &str = "your tap";
#[cfg(any(test, feature = "test-support"))]
const NPM_TOKEN_SECRET: &str = "NPM_TOKEN";
#[cfg(any(test, feature = "test-support"))]
const STANDING_LABEL_ORDER: [&str; 3] = [LABEL_DOCKER_HUB, LABEL_AUR, LABEL_CARGO_ALTERNATE];

#[cfg(any(test, feature = "test-support"))]
enum StandingSecretClass {
    Excluded,
    Label(String),
    Unrecognized,
}

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
fn step_emission_text(step: &Value) -> String {
    let mut collected = String::new();
    walk_values(step, &mut |value| {
        if let Some(text) = value.as_str() {
            collected.push_str(text);
            collected.push('\n');
        }
    });
    collected
}

#[cfg(any(test, feature = "test-support"))]
fn secret_reads_in_step(step: &Value) -> BTreeSet<String> {
    let mut secrets = BTreeSet::new();
    walk_values(step, &mut |value| {
        if let Some(text) = value.as_str() {
            let mut remaining = text;
            while let Some(open) = remaining.find("${{") {
                remaining = &remaining[open + 3..];
                let Some(close) = remaining.find("}}") else {
                    break;
                };
                let expression = &remaining[..close];
                for tail in expression.split("secrets.").skip(1) {
                    let name = tail
                        .chars()
                        .take_while(|character| {
                            character.is_ascii_alphanumeric() || *character == '_'
                        })
                        .collect::<String>();
                    if !name.is_empty() {
                        secrets.insert(name);
                    }
                }
                remaining = &remaining[close + 2..];
            }
        }
    });
    secrets
}

#[cfg(any(test, feature = "test-support"))]
fn is_destination_token_mint_step(step: &Value) -> bool {
    step.get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.contains("destination_token"))
}

#[cfg(any(test, feature = "test-support"))]
fn is_app_mint_credential(secret_name: &str) -> bool {
    secret_name.ends_with("GITHUB_APP_ID") || secret_name.ends_with("GITHUB_APP_PRIVATE_KEY")
}

#[cfg(any(test, feature = "test-support"))]
fn is_cargo_registry_token(secret_name: &str) -> bool {
    secret_name == "CARGO_REGISTRY_TOKEN" || secret_name.ends_with("_CARGO_REGISTRY_TOKEN")
}

#[cfg(any(test, feature = "test-support"))]
fn is_excluded_standing_secret_read(
    secret_name: &str,
    step: &Value,
    step_text: &str,
    job_text: &str,
) -> bool {
    if secret_name == "GITHUB_TOKEN" {
        return true;
    }
    if is_destination_token_mint_step(step) {
        return true;
    }
    if is_app_token_step(step) && is_app_mint_credential(secret_name) {
        return true;
    }
    if trusted_publishing_bootstrap_emission(step_text) && is_cargo_registry_token(secret_name) {
        return true;
    }
    if (secret_name == NPM_TOKEN_SECRET || secret_name.ends_with("_NPM_TOKEN"))
        && (trusted_publishing_bootstrap_emission(step_text)
            || job_text.contains(TRUSTED_PUBLISHING_TOKENS)
            || job_text.contains(NPM_TRUSTED_PREP_STEP))
    {
        return true;
    }
    if is_cargo_registry_token(secret_name) && job_text.contains(TRUSTED_PUBLISHING_TOKENS) {
        return true;
    }
    false
}

#[cfg(any(test, feature = "test-support"))]
fn standing_route_label(job_id: &str, job_text: &str) -> Option<&'static str> {
    if job_id.ends_with("_oci_dockerhub") {
        return Some(LABEL_DOCKER_HUB);
    }
    if job_id.ends_with("_aur_primary") {
        return Some(LABEL_AUR);
    }
    if job_id.ends_with("_cargo_primary") && !job_text.contains(TRUSTED_PUBLISHING_TOKENS) {
        return Some(LABEL_CARGO_ALTERNATE);
    }
    if job_id.ends_with("_homebrew_primary") {
        return Some(LABEL_HOMEBREW_TAP);
    }
    None
}

#[cfg(any(test, feature = "test-support"))]
fn classify_standing_secret_read(
    secret_name: &str,
    step: &Value,
    step_text: &str,
    job_text: &str,
    job_id: &str,
) -> StandingSecretClass {
    if is_excluded_standing_secret_read(secret_name, step, step_text, job_text) {
        return StandingSecretClass::Excluded;
    }
    match standing_route_label(job_id, job_text) {
        Some(label) => StandingSecretClass::Label(label.to_owned()),
        None => StandingSecretClass::Unrecognized,
    }
}

#[cfg(any(test, feature = "test-support"))]
fn standing_labels_from_job(job_id: &str, body: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut labels = BTreeSet::new();
    let mut unrecognized = BTreeSet::new();
    let job_text = emission_text(body);
    let job_level = body.as_mapping().map(|mapping| {
        let mut mapping = mapping.clone();
        mapping.remove(Value::String("steps".to_owned()));
        Value::Mapping(mapping)
    });
    if let Some(job_level) = job_level {
        let job_level_text = step_emission_text(&job_level);
        for secret_name in secret_reads_in_step(&job_level) {
            match classify_standing_secret_read(
                &secret_name,
                &job_level,
                &job_level_text,
                &job_text,
                job_id,
            ) {
                StandingSecretClass::Excluded => {}
                StandingSecretClass::Label(label) => {
                    labels.insert(label);
                }
                StandingSecretClass::Unrecognized => {
                    unrecognized.insert(secret_name);
                }
            }
        }
    }
    let Some(steps) = body.get("steps").and_then(Value::as_sequence) else {
        return (labels, unrecognized);
    };
    for step in steps {
        let step_text = step_emission_text(step);
        for secret_name in secret_reads_in_step(step) {
            match classify_standing_secret_read(&secret_name, step, &step_text, &job_text, job_id) {
                StandingSecretClass::Excluded => {}
                StandingSecretClass::Label(label) => {
                    labels.insert(label);
                }
                StandingSecretClass::Unrecognized => {
                    unrecognized.insert(secret_name);
                }
            }
        }
    }
    (labels, unrecognized)
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
    let mut unrecognized = BTreeSet::new();
    for (job_id, body) in jobs {
        let job_id = job_id.as_str().expect("publish job id");
        let managed = body["steps"].as_sequence().is_some_and(|steps| {
            steps
                .iter()
                .any(|step| step["id"].as_str() == Some(crate::executor::OWNERSHIP_SENTINEL))
        });
        if !managed {
            continue;
        }
        let (job_labels, job_unrecognized) = standing_labels_from_job(job_id, body);
        labels.extend(job_labels);
        unrecognized.extend(job_unrecognized);
    }
    assert!(
        unrecognized.is_empty(),
        "unrecognized stored credentials in managed publish-workflow emission cannot be classified for the standing population: {unrecognized:?}"
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_secret_reads_inside_composed_and_compact_expressions() {
        let step: Value = serde_yaml::from_str(
            "env:\n  URL: https://account:${{secrets.DELIVERY_TOKEN}}@example.invalid/\n  FALLBACK: ${{ secrets.PRIMARY_TOKEN || secrets.SECONDARY_TOKEN }}\n",
        )
        .expect("step parses");
        assert_eq!(
            secret_reads_in_step(&step),
            ["DELIVERY_TOKEN", "PRIMARY_TOKEN", "SECONDARY_TOKEN"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            "every secret expression is visible to the standing-credential census"
        );
    }

    #[test]
    fn finds_secret_reads_at_job_level_and_outside_publisher_jobs() {
        let publisher: Value = serde_yaml::from_str(
            "env:\n  TOKEN: ${{ secrets.DELIVERY_TOKEN }}\nsteps:\n  - id: intentional_executor_contract\n",
        )
        .expect("publisher parses");
        let (labels, unrecognized) =
            standing_labels_from_job("custom_publish_package_oci_dockerhub", &publisher);
        assert_eq!(labels, [LABEL_DOCKER_HUB.to_owned()].into_iter().collect());
        assert!(unrecognized.is_empty());

        let verifier: Value = serde_yaml::from_str(
            "env:\n  TOKEN: prefix-${{secrets.UNEXPECTED_TOKEN}}\nsteps:\n  - id: custom_executor_contract\n",
        )
        .expect("verifier parses");
        let (labels, unrecognized) =
            standing_labels_from_job("custom_verify_publications", &verifier);
        assert!(labels.is_empty());
        assert_eq!(
            unrecognized,
            ["UNEXPECTED_TOKEN".to_owned()].into_iter().collect()
        );
    }

    #[test]
    fn excludes_app_mint_credentials_only_on_the_app_token_route() {
        let ordinary: Value = serde_yaml::from_str(
            "env:\n  VALUE: ${{ secrets.UNRELATED_GITHUB_APP_PRIVATE_KEY }}\n",
        )
        .expect("step parses");
        let ordinary_text = step_emission_text(&ordinary);
        assert!(matches!(
            classify_standing_secret_read(
                "UNRELATED_GITHUB_APP_PRIVATE_KEY",
                &ordinary,
                &ordinary_text,
                &ordinary_text,
                "intentional_publish_unknown_primary",
            ),
            StandingSecretClass::Unrecognized
        ));

        let mint: Value = serde_yaml::from_str(
            "uses: actions/create-github-app-token@0123456789abcdef\nwith:\n  private-key: ${{ secrets.INTENTIONAL_GITHUB_APP_PRIVATE_KEY }}\n",
        )
        .expect("step parses");
        let mint_text = step_emission_text(&mint);
        assert!(matches!(
            classify_standing_secret_read(
                "INTENTIONAL_GITHUB_APP_PRIVATE_KEY",
                &mint,
                &mint_text,
                &mint_text,
                "intentional_publish_example_homebrew_primary",
            ),
            StandingSecretClass::Excluded
        ));
    }
}
