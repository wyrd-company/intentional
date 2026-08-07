// ---
// relationships:
//   tests: github-release-executor
// ---

use intentional_core::executor::fixture::{
    long_lived_repository_write_credentials, managed_release_tag_namespace_patterns,
    prefix_derived_repository_settings, release_managed_job_ids, resolved_publication_identities,
    standing_credential_usage_labels, trusted_publishing_bootstrap_route_count,
};
use intentional_core::executor::recipe::StoredCredentialKind;

fn executor_guide() -> String {
    std::fs::read_to_string("../../docs/executor.md").expect("executor guide is readable")
}

fn prepare_repository_section(usage: &str) -> &str {
    usage
        .split_once("## Prepare the repository")
        .expect("executor guide has the prepare section")
        .1
        .split_once("## Publish to a registry for the first time")
        .expect("prepare section ends before the registry section")
        .0
}

fn configure_executor_section(usage: &str) -> &str {
    usage
        .split_once("## Configure the GitHub executor")
        .expect("executor guide has the configure section")
        .1
        .split_once("## Reconcile the managed workflow slices")
        .expect("configure section ends before reconciliation")
        .0
}

fn authority_split_section(usage: &str) -> &str {
    usage
        .split_once("## Read the authority split in the maintained slice")
        .expect("executor guide has the authority split section")
        .1
        .split_once("## Prepare the repository")
        .expect("authority split section ends before repository preparation")
        .0
}

fn registry_section(usage: &str) -> &str {
    usage
        .split_once("## Publish to a registry for the first time")
        .expect("executor guide has the registry section")
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
fn usage_splits_executor_guidance_and_states_consumer_ownership() {
    let usage = std::fs::read_to_string("../../docs/usage.md").expect("usage guide is readable");
    let executor = executor_guide();

    assert!(
        !usage.contains("## Configure the GitHub executor")
            && executor.contains("## Configure the GitHub executor"),
        "executor operation lives in its own guide rather than core usage"
    );
    let normalized_executor = normalize_whitespace(&executor);
    assert!(
        normalized_executor.contains("You own the workflow documents")
            && normalized_executor
                .contains("Intentional generates complete authority-bearing slices")
            && !executor.contains("Intentional owns"),
        "the executor guide assigns workflow ownership to the consumer"
    );
}

#[test]
fn usage_names_the_derived_long_lived_repository_write_credentials() {
    let usage = executor_guide();
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
    let usage = executor_guide();
    let section = registry_section(&usage);
    let bootstrap_section = section
        .split_once("### npmjs and crates.io start with a token and stop using it")
        .expect("registry section has the bootstrap subsection")
        .1
        .split_once("### Standing credentials")
        .expect("bootstrap subsection ends before standing credentials")
        .0;
    let bootstrap_route_count = trusted_publishing_bootstrap_route_count();

    assert_eq!(
        bootstrap_route_count, 2,
        "trusted publishing bootstraps exactly npmjs and crates.io in emitted workflows"
    );
    assert!(
        normalize_whitespace(bootstrap_section)
            .contains("Both publish through registry trusted publishing"),
        "the bootstrap subsection names trusted publishing for npmjs and crates.io"
    );
    assert!(
        normalize_whitespace(bootstrap_section).contains("Both recipes separate the outcomes"),
        "the bootstrap subsection names npmjs and crates.io probe separation for both routes"
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
    let usage = executor_guide();
    let labels = standing_credential_usage_labels();
    let standing_section = registry_section(&usage)
        .split_once("### Standing credentials")
        .expect("registry section has standing credentials")
        .1
        .split_once("### Credentials you do not store")
        .expect("standing credentials subsection ends before job-token credentials")
        .0;
    let expected = format!(
        "{} authenticate every publication with the credential you stored",
        join_usage_labels(&labels.iter().map(String::as_str).collect::<Vec<_>>())
    );

    assert_eq!(
        labels.len(),
        3,
        "three standing-credential destinations are derived from emitted publish jobs: {labels:?}"
    );
    assert!(
        normalize_whitespace(standing_section).contains(&expected),
        "the standing-credential sentence names every derived destination: {expected}"
    );
}

#[test]
fn usage_executor_check_resolves_every_configured_publication_to_one_recipe() {
    let usage = executor_guide();
    let section = configure_executor_section(&usage);
    let publications = resolved_publication_identities();

    assert!(
        normalize_whitespace(section).contains(
            "The check resolves every configured publication to exactly one maintained recipe"
        ),
        "the configure section documents one-recipe resolution"
    );
    assert!(
        !publications.is_empty(),
        "the exhaustive fixture declares configured publications to resolve"
    );
}

#[test]
fn usage_release_workflow_derives_two_managed_jobs() {
    let usage = executor_guide();
    let section = authority_split_section(&usage);
    let managed_jobs = release_managed_job_ids();

    assert!(
        normalize_whitespace(section).contains("The release workflow derives two managed jobs"),
        "the authority split section names the managed job count"
    );
    assert_eq!(
        managed_jobs.len(),
        2,
        "the derived release workflow owns exactly preparation and release jobs: {managed_jobs:?}"
    );
}

#[test]
fn usage_ruleset_bypass_claim_covers_every_managed_release_tag_namespace() {
    let usage = executor_guide();
    let configure = configure_executor_section(&usage);
    let prepare = prepare_repository_section(&usage);
    let namespaces = managed_release_tag_namespace_patterns();
    let bypass_claim = "every managed release tag namespace";

    assert!(
        normalize_whitespace(configure).contains(bypass_claim),
        "initialization reports bypass for every managed release tag namespace"
    );
    assert!(
        normalize_whitespace(prepare).contains(bypass_claim),
        "repository preparation reports bypass for every managed release tag namespace"
    );
    assert!(
        !namespaces.is_empty(),
        "the exhaustive fixture derives managed release tag namespaces to enumerate"
    );
}

#[test]
fn usage_prefix_derived_repository_setting_names_match_configuration() {
    let usage = executor_guide();
    let section = prepare_repository_section(&usage);
    let settings = prefix_derived_repository_settings();

    assert!(
        normalize_whitespace(section).contains(
            "the App private key secret and the AUR key secret all derive from the `envvar`"
        ),
        "the prepare section documents prefix-derived credential names"
    );
    assert!(
        section.contains(&format!("`{}`", settings.environment)),
        "the derived protected environment is named in the prepare section"
    );
    assert!(
        section.contains(&format!("`{}`", settings.app_id_variable)),
        "the derived App ID variable is named in the prepare section"
    );
    assert!(
        section.contains(&format!("`{}`", settings.app_private_key_secret)),
        "the derived App private key secret is named in the prepare section"
    );
    assert!(
        usage.contains(&format!("`{}`", settings.aur_key_secret)),
        "the derived AUR key secret is named in the executor guide"
    );
}
