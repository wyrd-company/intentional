// ---
// relationships:
//   tests: github-release-executor
// ---

use intentional_core::executor::fixture::{
    long_lived_repository_write_credentials, standing_credential_destinations,
    trusted_publishing_bootstrap_destinations,
};
use intentional_core::executor::recipe::StoredCredentialKind;

fn usage_guide() -> String {
    std::fs::read_to_string("../../docs/usage.md").expect("usage guide is readable")
}

fn prepare_repository_section(usage: &str) -> &str {
    usage
        .split_once("## Prepare the repository")
        .expect("usage guide has the prepare section")
        .1
        .split_once("## Publish to a registry for the first time")
        .expect("prepare section ends before the registry section")
        .0
}

fn registry_section(usage: &str) -> &str {
    usage
        .split_once("## Publish to a registry for the first time")
        .expect("usage guide has the registry section")
        .1
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sentences(text: &str) -> Vec<String> {
    text.split('.')
        .map(normalize_whitespace)
        .filter(|sentence| !sentence.is_empty())
        .collect()
}
fn join_usage_labels(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [one] => one.to_string(),
        [first, second] => format!("{first} and {second}"),
        rest => {
            let (last, head) = (rest[rest.len() - 1], &rest[..rest.len() - 1]);
            format!("{}, and {last}", head.join(", "))
        }
    }
}

#[test]
fn usage_names_the_derived_long_lived_repository_write_credentials() {
    let usage = usage_guide();
    let credentials = long_lived_repository_write_credentials();
    let section = prepare_repository_section(&usage);
    let variable = credentials
        .iter()
        .find(|(kind, _)| *kind == StoredCredentialKind::RepositoryVariable)
        .expect("one repository variable is derived");
    let secret = credentials
        .iter()
        .find(|(kind, _)| *kind == StoredCredentialKind::RepositorySecret)
        .expect("one repository secret is derived");
    let exclusivity_claims = sentences(section)
        .into_iter()
        .filter(|sentence| sentence.ends_with("long-lived repository-write credentials involved"))
        .collect::<Vec<_>>();

    assert!(
        section.contains(&format!("`{}` as a repository **variable**", variable.1)),
        "the derived App ID variable is named as a repository variable"
    );
    assert!(
        section.contains(&format!("`{}` as a repository **secret**", secret.1)),
        "the derived App private key secret is named as a repository secret"
    );
    assert_eq!(
        exclusivity_claims,
        ["Together they are the only long-lived repository-write credentials involved"],
        "one exclusivity sentence matches the derived credential population"
    );
    assert_eq!(
        credentials.len(),
        2,
        "the derived population is exactly the App ID variable and private key secret"
    );
}

#[test]
fn usage_documents_bootstrap_properties_without_exhaustive_counts() {
    let usage = usage_guide();
    let section = registry_section(&usage);
    let bootstrap_section = section
        .split_once("### npmjs and crates.io start with a token and stop using it")
        .expect("registry section has the bootstrap subsection")
        .1
        .split_once("### Standing credentials")
        .expect("bootstrap subsection ends before standing credentials")
        .0;
    let bootstrap_destinations = trusted_publishing_bootstrap_destinations();

    assert_eq!(
        bootstrap_destinations.len(),
        2,
        "trusted publishing bootstraps exactly npmjs and crates.io"
    );
    assert!(
        !bootstrap_section.contains("Two properties hold for these two destinations"),
        "the bootstrap subsection does not restate exhaustive counts the bold paragraphs already carry"
    );
    assert!(
        normalize_whitespace(bootstrap_section).contains(
            "bootstrap path opens only on proof that the destination does not hold the package"
        ),
        "the bootstrap probe property is documented"
    );
    assert!(
        normalize_whitespace(bootstrap_section)
            .contains("requires the trusted identity and never falls back"),
        "the trusted-identity property is documented"
    );
}

#[test]
fn usage_standing_credential_sentence_matches_derived_destinations() {
    let usage = usage_guide();
    let destinations = standing_credential_destinations();
    let labels = destinations
        .iter()
        .map(|destination| destination.usage_label())
        .collect::<Vec<_>>();
    let standing_section = registry_section(&usage)
        .split_once("### Standing credentials")
        .expect("registry section has standing credentials")
        .1
        .split_once("### Credentials you do not store")
        .expect("standing credentials subsection ends before job-token credentials")
        .0;
    let expected = format!(
        "{} authenticate every publication with the credential you stored",
        join_usage_labels(&labels)
    );

    assert_eq!(
        destinations.len(),
        3,
        "three standing-credential destinations are derived"
    );
    assert!(
        normalize_whitespace(standing_section).contains(&expected),
        "the standing-credential sentence names every derived destination: {expected}"
    );
}
