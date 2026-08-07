// ---
// relationships:
//   implements: github-release-executor
// ---

// Publisher-job derivation moved from `executor::workflow` so publication
// routes can change independently from workflow reconciliation.

use super::*;

/// Jobs derived from one resolved publication and its authority boundaries.
pub(super) struct PublicationJobs {
    /// Destination mutation under the authority publication requires.
    pub(super) publisher: Value,
    /// Stable identity of the credential-separated verification job.
    pub(super) verification_id: String,
    /// Evidence verification after destination authority has left the runner.
    pub(super) verification: Value,
}

/// Publisher job and credential-separated verification for one publication.
pub(super) fn publication_jobs(
    root: &Path,
    namespaces: &PrefixNamespaces,
    needs: &[String],
    publication: &SelectedPublication,
    subject: &DistinctSubject,
    config: &Config,
    handoff_slug: Option<&str>,
) -> std::result::Result<PublicationJobs, WorkflowDiagnostic> {
    let unit = &config.release_units[&publication.release_unit];
    // An omitted selector is what chooses an adapter's configured primary
    // destination, so a primary publication passes the Action an empty selector
    // rather than naming `primary` as a target it would have to resolve.
    let target = if publication.target == PRIMARY_TARGET {
        String::new()
    } else {
        publication.target.clone()
    };
    let identity = publication.identity();
    let slug = identifier(&identity);
    // Publisher credentials stay in the repository-owned recipe steps, so the
    // destination readback they perform reaches the portable command as a
    // schema-backed observation rather than as a second verification path.
    let observation = format!(
        "${{{{ runner.temp }}}}/{}observation/{slug}.yml",
        namespaces.job
    );
    let evidence = format!(
        "${{{{ runner.temp }}}}/{}evidence/{slug}.yml",
        namespaces.job
    );
    let delivery_namespace = namespaces
        .job
        .trim_end_matches('_')
        .replace('_', "-")
        .to_lowercase();
    // The recipe's own steps are written by the module that owns each
    // destination, and they read the sealed subject from the build job's
    // outputs rather than from a value they assert, so the observation they
    // produce is bound to the subject the phase tag sealed.
    let recipe = crate::executor::steps::recipe_steps(&crate::executor::steps::RecipeContext {
        publication,
        unit,
        subject_identity: &subject.identity,
        build_job: &format!("{}build_{}", namespaces.job, subject.slug),
        working_directory: &subject.working_directory,
        observation: &observation,
        root,
        work: &format!("${{{{ runner.temp }}}}/{}readback/{slug}", namespaces.job),
        delivery_namespace: &delivery_namespace,
    })
    .map_err(|refusal| {
        let path = refusal
            .path
            .unwrap_or_else(|| format!("release-units.{}", publication.release_unit));
        WorkflowDiagnostic::at(refusal.code, refusal.message, &path)
    })?;
    let mut observation_inputs = recipe.observation_inputs.clone();
    let required_clients = recipe
        .observer_clients
        .iter()
        .map(|client| client.name())
        .collect::<Vec<_>>()
        .join(" ");
    if !required_clients.is_empty() {
        observation_inputs.push_str(&format!(
            "      required-clients: {}\n",
            scalar(&required_clients)
        ));
    }
    let observation_client_steps = recipe
        .observer_clients
        .iter()
        .map(|client| client.installer())
        .collect::<String>();
    let observation_inline = recipe.observation_inline;
    let verification = |handoff: &str| {
        PUBLISH_VERIFY_STEPS
            .replace(
                "@VERIFY_NAME@",
                &scalar(&format!("Verify the {identity} publication")),
            )
            .replace(
                "@FRAGMENT_NAME@",
                &scalar(&format!("Upload the {identity} evidence fragment")),
            )
            .replace("@RELEASE_UNIT@", &scalar(&publication.release_unit))
            .replace("@PACKAGE@", &scalar(&publication.package))
            .replace("@PUBLISHER@", &scalar(publication.publisher.as_str()))
            .replace("@TARGET@", &scalar(&target))
            .replace("@OBSERVATION@", &scalar(&observation))
            .replace("@OUTPUT@", &scalar(&evidence))
            .replace("@HANDOFF@", &scalar(handoff))
            .replace("@OBSERVATION_INPUTS@", &observation_inputs)
            .replace("@SLUG@", &slug)
    };
    let subject_name = scalar(&format!("Download the built {} subject", subject.identity));
    let publisher_steps = recipe.publisher;
    let retrieval_steps = recipe.retrieval;
    let mut substitutions = vec![
        ("@NEEDS@", render_list(needs)),
        ("@SUBJECT_SLUG@", subject.slug.clone()),
    ];
    // A draft-dependent publisher's fragment records what it retrieved from the
    // draft Release, and that claim is proved against the inventory the release
    // sealed for this publication. The document arrives as the artifact the
    // upload job wrote it to, at the path the same expressions derive on both
    // sides, so the job consumes the handoff for its own publication rather
    // than whichever document happens to be on the runner. A publisher whose
    // consumer path reads no draft asset receives none, and the Action turns an
    // empty input into an absent option rather than an empty path.
    let handoff = handoff_slug.map_or_else(String::new, |slug| handoff_file(namespaces, slug));
    let split_retrieval = retrieval_steps.is_some();
    let handoff_step = handoff_slug.map_or_else(String::new, |slug| {
        format!(
            "  - name: {}\n    uses: @DOWNLOAD@\n    with:\n      name: {}\n      path: {}\n",
            scalar(&format!("Download the {identity} draft-asset handoff")),
            handoff_artifact(namespaces, slug),
            handoff_directory(namespaces, slug),
        )
    });
    let observation_artifact = format!("{}observation-{slug}", namespaces.job);
    let observation_upload_step = if observation_inline && !split_retrieval {
        format!(
            "  - name: {}\n    uses: @UPLOAD@\n    with:\n      name: {}\n      path: {}\n      retention-days: 1\n",
            scalar(&format!("Upload the {identity} publication observation")),
            observation_artifact,
            scalar(&observation),
        )
    } else {
        String::new()
    };
    substitutions.extend([
        // Recipe-emitted steps are repository-derived text and are substituted
        // in the middle of this list, so their position would matter if they
        // could name another entry's placeholder. They cannot: the renderer
        // refuses a value that names a later substitution, which is what makes
        // this position a free choice rather than a contract.
        ("@RECIPE_STEPS@", publisher_steps),
        ("@SUBJECT_NAME@", subject_name.clone()),
        ("@PERMISSIONS@", publisher_permissions(publication)),
        ("@OBSERVATION_UPLOAD_STEP@", observation_upload_step),
    ]);
    let publisher = job(
        PUBLISH_PUBLISHER_JOB,
        namespaces,
        &substitutions
            .iter()
            .map(|(placeholder, value)| (*placeholder, value.as_str()))
            .collect::<Vec<_>>(),
    )?;
    let verifier_needs = vec![
        publication_job_id(namespaces, publication),
        format!("{}build_{}", namespaces.job, subject.slug),
    ];
    let verifier_needs = render_list(&verifier_needs);
    let verifier_verification = verification(&handoff);
    let verification_id = if split_retrieval {
        retrieval_job_id(namespaces, publication)
    } else {
        format!("{}verify_{slug}", namespaces.job)
    };
    let observation_step = if observation_inline && !split_retrieval {
        format!(
            "  - name: {}\n    uses: @DOWNLOAD@\n    with:\n      name: {}\n      path: ${{{{ runner.temp }}}}/{}observation\n",
            scalar(&format!("Download the {identity} publication observation")),
            observation_artifact,
            namespaces.job,
        )
    } else {
        String::new()
    };
    let packages_permission = if matches!(
        (publication.publisher, publication.target.as_str()),
        (PublisherKind::Npm, "github")
    ) {
        "  packages: read\n"
    } else {
        ""
    };
    let retrieval_steps = retrieval_steps.unwrap_or_default();
    let verification = job(
        PUBLISH_VERIFICATION_JOB,
        namespaces,
        &[
            ("@NEEDS@", verifier_needs.as_str()),
            ("@PACKAGES_PERMISSION@", packages_permission),
            ("@SUBJECT_SLUG@", subject.slug.as_str()),
            ("@SUBJECT_NAME@", subject_name.as_str()),
            ("@HANDOFF_STEP@", handoff_step.as_str()),
            ("@OBSERVATION_STEP@", observation_step.as_str()),
            (
                "@OBSERVATION_CLIENT_STEPS@",
                observation_client_steps.as_str(),
            ),
            ("@RETRIEVAL_STEPS@", retrieval_steps.as_str()),
            ("@VERIFY_STEPS@", verifier_verification.as_str()),
        ],
    )?;
    Ok(PublicationJobs {
        publisher,
        verification_id,
        verification,
    })
}

/// Least privilege one publication's destination requires.
///
/// The workflow-identity scope is granted to the destinations whose recipes
/// exchange that identity for something: a registry trusted-publishing token,
/// or a provenance attestation bound to the run. A destination that
/// authenticates with a repository-scoped or configured token exchanges nothing
/// and is derived without it, because a job that holds a workflow identity it
/// never presents is a credential sitting in reach of every step in it.
fn publisher_permissions(publication: &SelectedPublication) -> String {
    let mut scopes = vec!["  contents: read\n".to_owned()];
    let packages = matches!(
        (publication.publisher, publication.target.as_str()),
        (PublisherKind::Npm, "github") | (PublisherKind::Oci, "ghcr")
    );
    if packages {
        scopes.push("  packages: write\n".to_owned());
    }
    if presents_a_workflow_identity(publication) {
        scopes.push("  id-token: write\n".to_owned());
    }
    scopes.concat()
}

/// Whether one publication's recipe presents the run's workflow identity.
///
/// Every adapter states its own answer rather than sharing a catch-all. The
/// adapters below npm and Cargo are owned by separate tasks working from a
/// common base, and a catch-all makes an answer each of them has to give
/// separately into one they have to change together.
fn presents_a_workflow_identity(publication: &SelectedPublication) -> bool {
    match (publication.publisher, publication.target.as_str()) {
        // GitHub Package Registry implements neither trusted publishing nor
        // provenance attestation; its recipe presents the workflow token.
        (PublisherKind::Npm, "github") => false,
        (PublisherKind::Npm, _) => true,
        // An alternate Cargo registry defines its own trusted publishing, if
        // any, so the maintained recipe authenticates it with a configured
        // token. Only crates.io performs the identity exchange.
        (PublisherKind::Cargo, _) => publication.destination.as_deref() == Some("crates.io"),
        // A tap, a package index, and the Arch User Repository accept no
        // workflow identity: they are repositories, reached with a narrowly
        // scoped installation token or an SSH key. Granting the scope anyway
        // would widen every one of these jobs for nothing.
        (PublisherKind::Homebrew, _) => false,
        (PublisherKind::Rpm, _) => false,
        (PublisherKind::Apt, _) => false,
        (PublisherKind::Aur, _) => false,
        (PublisherKind::Oci, _) => publication
            .components
            .contains(&crate::model::AttachedComponent::Signature),
    }
}
