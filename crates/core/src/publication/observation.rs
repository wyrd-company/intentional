// ---
// relationships:
//   implements: github-release-executor
// ---

//! Destination observation and bounded eventual-consistency recovery.

use crate::error::{Error, Result};
use crate::evidence::assemble::{
    AttachedMetadata, CleanClient, CleanClientMode, Destination, DestinationAlias,
    EvidenceReference, PackagerRecord, Subject,
};
use crate::model::PublisherKind;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

/// Schema identity of one publication observation.
pub const PUBLICATION_OBSERVATION_SCHEMA: &str =
    "https://intentional.foo/schemas/publication-observation/v1";
/// Contract identity of one publication observation.
pub const PUBLICATION_OBSERVATION_CONTRACT: &str = "publication-observation-1";

/// What a repository-local readback found at one destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationState {
    /// The expected release is visible and was retrieved.
    Present,
    /// The destination accepted the publication but has not made it observable.
    Pending,
    /// The destination holds no release under the expected identity.
    Absent,
    /// The destination holds a release that disagrees with the expected one.
    Conflict,
}

impl ObservationState {
    /// Stable state name used in diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Pending => "pending",
            Self::Absent => "absent",
            Self::Conflict => "conflict",
        }
    }
}

impl std::fmt::Display for ObservationState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One repository-local readback of one configured publication.
///
/// Secret-bearing readback runs in repository-local recipe steps, so this
/// document is everything the credential-free verification command knows about
/// a destination. It carries no wall-clock value, because the fragment derived
/// from it must be byte-identical across runs and across runners.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PublicationObservation {
    /// Publication observation schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Publication observation contract identity.
    pub contract: String,
    /// Release unit the observed publication belongs to.
    pub release_unit: String,
    /// Publisher adapter that performed the publication.
    pub publisher: PublisherKind,
    /// Canonical target identity the recipe observed.
    pub target: String,
    /// What the readback found.
    pub state: ObservationState,
    /// Subject the destination holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    /// Packager that produced the observed subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packager: Option<PackagerRecord>,
    /// Native build provenance the recipe produced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build_provenance: Vec<EvidenceReference>,
    /// Components the recipe actually attached to the subject.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_metadata: Vec<AttachedMetadata>,
    /// Identity, version, and digest read back from the destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Destination>,
    /// Closure-time consumer retrieval the recipe performed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<CleanClient>,
    /// Mutable aliases read back after promotion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destination_aliases: Vec<DestinationAlias>,
    /// Identity, checksum, or digest that disagrees with the expected release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
}

impl PublicationObservation {
    /// Stable publication identity this observation describes.
    pub fn identity(&self) -> String {
        format!("{}/{}/{}", self.release_unit, self.publisher, self.target)
    }

    /// Read and validate one observation document.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        let observation: Self = serde_yaml::from_str(&text).map_err(|error| {
            Error::Validation(format!(
                "publication observation {} is not schema-valid: {error}",
                path.display()
            ))
        })?;
        observation.validate(&path.display().to_string())?;
        Ok(observation)
    }

    /// Report the first disagreement between the observed state and its members.
    fn validate(&self, label: &str) -> Result<()> {
        if self.schema != PUBLICATION_OBSERVATION_SCHEMA {
            return Err(Error::Validation(format!(
                "{label} declares $schema {:?} instead of {PUBLICATION_OBSERVATION_SCHEMA}",
                self.schema
            )));
        }
        if self.contract != PUBLICATION_OBSERVATION_CONTRACT {
            return Err(Error::Validation(format!(
                "{label} declares contract {:?} instead of {PUBLICATION_OBSERVATION_CONTRACT}",
                self.contract
            )));
        }
        let observed = [
            ("subject", self.subject.is_some()),
            ("packager", self.packager.is_some()),
            ("destination", self.destination.is_some()),
            ("retrieval", self.retrieval.is_some()),
            ("build-provenance", !self.build_provenance.is_empty()),
            ("attached-metadata", !self.attached_metadata.is_empty()),
            ("destination-aliases", !self.destination_aliases.is_empty()),
        ];
        match self.state {
            ObservationState::Present => {
                for member in ["subject", "packager", "destination", "retrieval"] {
                    if !observed
                        .iter()
                        .any(|(name, supplied)| *name == member && *supplied)
                    {
                        return Err(Error::Validation(format!(
                            "{label} observes state present without {member}; a present observation records the subject, packager, destination readback, and retrieval"
                        )));
                    }
                }
            }
            ObservationState::Pending | ObservationState::Absent => {
                if let Some((member, _)) = observed.iter().find(|(_, supplied)| *supplied) {
                    return Err(Error::Validation(format!(
                        "{label} observes state {} and carries {member}; an unobservable publication records no destination detail",
                        self.state
                    )));
                }
            }
            ObservationState::Conflict => {}
        }
        // Every mode but this one records the retrieved bytes in the client's
        // own form, which the protocol cannot interpret. An authenticated-draft
        // retrieval is the exception: its bytes are a draft Release asset, and
        // the draft-asset handoff verifies exactly those bytes against the
        // canonical inventory the release sealed. Requiring the same spelling
        // here is what keeps joining the two a comparison rather than a
        // translation.
        if let Some(retrieval) = &self.retrieval {
            if retrieval.mode == CleanClientMode::AuthenticatedDraft
                && !crate::evidence::is_digest(&retrieval.digest)
            {
                return Err(Error::Validation(format!(
                    "{label} observes an authenticated-draft retrieval whose digest {:?} is not a canonical sha256 digest; those bytes are a draft Release asset and the release sealed their sha256",
                    retrieval.digest
                )));
            }
        }
        match (self.state, self.conflict.as_deref()) {
            (ObservationState::Conflict, Some(conflict)) if !conflict.trim().is_empty() => Ok(()),
            (ObservationState::Conflict, _) => Err(Error::Validation(format!(
                "{label} observes state conflict without a conflict description naming the identity, checksum, or digest that disagrees"
            ))),
            (state, Some(_)) => Err(Error::Validation(format!(
                "{label} observes state {state} and carries a conflict description; only a conflict observation describes one"
            ))),
            _ => Ok(()),
        }
    }
}

/// Monotonic time seam the observation loop advances through.
///
/// Observation is bounded by elapsed time rather than by attempt count, so the
/// loop reads both its progress and its waiting from one injected source and a
/// test drives the whole policy without waiting.
pub trait Clock {
    /// Time elapsed since observation began.
    fn elapsed(&self) -> Duration;

    /// Wait before the next re-read.
    fn wait(&self, duration: Duration);
}

/// Wall-clock observation timing.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Start measuring an observation from this moment.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }

    fn wait(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Bounded eventual-consistency policy of one publisher adapter.
///
/// Maintained destinations become observable eventually rather than
/// immediately, so the policy states how long that lag may last before the
/// workflow treats it as a failure worth retrying rather than as progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsistencyPolicy {
    /// Wait before the first re-read.
    pub interval: Duration,
    /// Factor the interval grows by after each pending re-read.
    pub backoff: u32,
    /// Longest wait backoff may reach.
    pub maximum_interval: Duration,
    /// Total time a publication may remain pending.
    pub deadline: Duration,
}

impl ConsistencyPolicy {
    /// Maintained policy for one publisher adapter.
    ///
    /// Registry-indexed destinations lag behind their own accepted writes far
    /// longer than repository destinations, whose visibility is a pushed
    /// commit, so the bound belongs to the adapter rather than to the target.
    pub const fn maintained(publisher: PublisherKind) -> Self {
        match publisher {
            PublisherKind::Npm | PublisherKind::Cargo => Self {
                interval: Duration::from_secs(5),
                backoff: 2,
                maximum_interval: Duration::from_secs(30),
                deadline: Duration::from_secs(600),
            },
            PublisherKind::Oci => Self {
                interval: Duration::from_secs(3),
                backoff: 2,
                maximum_interval: Duration::from_secs(15),
                deadline: Duration::from_secs(300),
            },
            PublisherKind::Homebrew
            | PublisherKind::Rpm
            | PublisherKind::Apt
            | PublisherKind::Aur => Self {
                interval: Duration::from_secs(5),
                backoff: 2,
                maximum_interval: Duration::from_secs(20),
                deadline: Duration::from_secs(300),
            },
        }
    }
}

/// Observe one publication until it is present or its policy is exhausted.
///
/// A destination that has not yet indexed an accepted publication is
/// indistinguishable from one that never received it, so a document the recipe
/// has not written yet reads as pending. Only the recipe's own `absent` claim,
/// made after a completed publish attempt, is a failure.
pub fn observe(
    path: &Path,
    identity: &str,
    policy: &ConsistencyPolicy,
    clock: &dyn Clock,
) -> Result<PublicationObservation> {
    let mut interval = policy.interval;
    loop {
        let observed = if path.exists() {
            Some(PublicationObservation::load(path)?)
        } else {
            None
        };
        match observed {
            Some(observation) if observation.state == ObservationState::Present => {
                return Ok(observation)
            }
            Some(observation) if observation.state == ObservationState::Conflict => {
                return Err(Error::Validation(format!(
                    "publication {identity} conflicts with its destination: {}",
                    observation.conflict.unwrap_or_default()
                )))
            }
            Some(observation) if observation.state == ObservationState::Absent => {
                return Err(Error::Validation(format!(
                    "publication {identity} is absent from its destination after a completed publish attempt"
                )))
            }
            _ => {}
        }
        let Some(remaining) = policy.deadline.checked_sub(clock.elapsed()) else {
            return Err(deadline_failure(identity, policy));
        };
        if remaining.is_zero() {
            return Err(deadline_failure(identity, policy));
        }
        clock.wait(interval.min(remaining));
        interval = interval
            .saturating_mul(policy.backoff)
            .min(policy.maximum_interval);
    }
}

/// Failure reported when a publication never became observable in time.
fn deadline_failure(identity: &str, policy: &ConsistencyPolicy) -> Error {
    Error::Validation(format!(
        "publication {identity} remained pending past its {} second observation deadline; the failure is retryable and a rerun reads the destination again before deciding",
        policy.deadline.as_secs()
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;
    use std::cell::{Cell, RefCell};

    /// Observation timing a test advances by hand.
    struct TestClock {
        elapsed: Cell<Duration>,
        waits: RefCell<Vec<Duration>>,
    }

    impl TestClock {
        fn new() -> Self {
            Self {
                elapsed: Cell::new(Duration::ZERO),
                waits: RefCell::new(Vec::new()),
            }
        }
    }

    impl Clock for TestClock {
        fn elapsed(&self) -> Duration {
            self.elapsed.get()
        }

        fn wait(&self, duration: Duration) {
            self.waits.borrow_mut().push(duration);
            self.elapsed.set(self.elapsed.get() + duration);
        }
    }

    fn policy() -> ConsistencyPolicy {
        ConsistencyPolicy {
            interval: Duration::from_secs(5),
            backoff: 2,
            maximum_interval: Duration::from_secs(20),
            deadline: Duration::from_secs(60),
        }
    }

    /// One digest-shaped value distinguishable from every other in a test.
    pub(crate) fn digest(marker: &str) -> String {
        format!("sha256:{marker}{}", "0".repeat(64 - marker.len()))
    }

    /// A complete present observation of the sample npm publication.
    pub(crate) fn present_document() -> String {
        format!(
            "$schema: {PUBLICATION_OBSERVATION_SCHEMA}
contract: {PUBLICATION_OBSERVATION_CONTRACT}
release-unit: component
publisher: npm
target: primary
state: present
subject:
  kind: npm-package
  identity: sample-library
  version: 1.2.3
  digest: {subject}
packager:
  id: npm
  version: 10.9.0
destination:
  identity: npmjs
  version: 1.2.3
  digest: {subject}
retrieval:
  mode: public
  client: npm
  version: 10.9.0
  digest: {subject}
",
            subject = digest("aa")
        )
    }

    fn pending_document() -> String {
        format!(
            "$schema: {PUBLICATION_OBSERVATION_SCHEMA}
contract: {PUBLICATION_OBSERVATION_CONTRACT}
release-unit: component
publisher: npm
target: primary
state: pending
"
        )
    }

    #[test]
    fn a_present_observation_is_accepted_without_waiting() {
        let workspace = Workspace::new("observe-present");
        workspace.write("observation.yml", &present_document());
        let clock = TestClock::new();
        let observed = observe(
            &workspace.root().join("observation.yml"),
            "component/npm/primary",
            &policy(),
            &clock,
        )
        .expect("a present observation is accepted");
        assert_eq!(observed.state, ObservationState::Present);
        assert_eq!(observed.identity(), "component/npm/primary");
        assert!(
            clock.waits.borrow().is_empty(),
            "an observable publication never waits"
        );
    }

    #[test]
    fn a_document_the_recipe_has_not_written_yet_stays_pending() {
        let workspace = Workspace::new("observe-missing");
        let path = workspace.root().join("observation.yml");
        let document = present_document();
        struct WritingClock<'a> {
            elapsed: Cell<Duration>,
            waits: Cell<u32>,
            path: &'a Path,
            document: &'a str,
        }
        impl Clock for WritingClock<'_> {
            fn elapsed(&self) -> Duration {
                self.elapsed.get()
            }

            fn wait(&self, duration: Duration) {
                self.elapsed.set(self.elapsed.get() + duration);
                self.waits.set(self.waits.get() + 1);
                std::fs::write(self.path, self.document).expect("observation appears");
            }
        }
        let clock = WritingClock {
            elapsed: Cell::new(Duration::ZERO),
            waits: Cell::new(0),
            path: &path,
            document: &document,
        };
        let observed = observe(&path, "component/npm/primary", &policy(), &clock)
            .expect("the observation is accepted once it appears");
        assert_eq!(observed.state, ObservationState::Present);
        assert_eq!(clock.waits.get(), 1, "the loop re-read the destination");
    }

    #[test]
    fn a_pending_observation_becomes_present_within_the_deadline() {
        let workspace = Workspace::new("observe-pending");
        let path = workspace.root().join("observation.yml");
        std::fs::write(&path, pending_document()).expect("pending observation");
        struct PromotingClock<'a> {
            elapsed: Cell<Duration>,
            waits: Cell<u32>,
            path: &'a Path,
            document: &'a str,
        }
        impl Clock for PromotingClock<'_> {
            fn elapsed(&self) -> Duration {
                self.elapsed.get()
            }

            fn wait(&self, duration: Duration) {
                self.elapsed.set(self.elapsed.get() + duration);
                self.waits.set(self.waits.get() + 1);
                if self.waits.get() == 3 {
                    std::fs::write(self.path, self.document).expect("observation advances");
                }
            }
        }
        let document = present_document();
        let clock = PromotingClock {
            elapsed: Cell::new(Duration::ZERO),
            waits: Cell::new(0),
            path: &path,
            document: &document,
        };
        let observed = observe(&path, "component/npm/primary", &policy(), &clock)
            .expect("a publication that becomes observable is accepted");
        assert_eq!(observed.state, ObservationState::Present);
        assert_eq!(clock.waits.get(), 3, "each pending read waited once");
        assert!(
            clock.elapsed() < policy().deadline,
            "the publication was accepted inside its deadline"
        );
    }

    #[test]
    fn a_pending_observation_past_the_deadline_is_a_retryable_failure() {
        let workspace = Workspace::new("observe-deadline");
        workspace.write("observation.yml", &pending_document());
        let clock = TestClock::new();
        let error = observe(
            &workspace.root().join("observation.yml"),
            "component/npm/primary",
            &policy(),
            &clock,
        )
        .expect_err("an exhausted deadline is reported");
        let message = error.to_string();
        assert!(
            message.contains("remained pending past its 60 second observation deadline"),
            "{message}"
        );
        assert!(message.contains("retryable"), "{message}");
        assert_eq!(
            *clock.waits.borrow(),
            vec![
                Duration::from_secs(5),
                Duration::from_secs(10),
                Duration::from_secs(20),
                Duration::from_secs(20),
                Duration::from_secs(5),
            ],
            "the interval backs off to the maximum and then to the remaining deadline"
        );
    }

    #[test]
    fn a_conflicting_observation_fails_without_a_further_attempt() {
        let workspace = Workspace::new("observe-conflict");
        workspace.write(
            "observation.yml",
            &format!(
                "$schema: {PUBLICATION_OBSERVATION_SCHEMA}
contract: {PUBLICATION_OBSERVATION_CONTRACT}
release-unit: component
publisher: npm
target: primary
state: conflict
conflict: >-
  sample-library 1.2.3 resolves to {found} while the release built {expected}
",
                found = digest("bb"),
                expected = digest("aa")
            ),
        );
        let clock = TestClock::new();
        let error = observe(
            &workspace.root().join("observation.yml"),
            "component/npm/primary",
            &policy(),
            &clock,
        )
        .expect_err("a conflict is reported");
        assert!(error.to_string().contains(&digest("bb")), "{error}");
        assert!(
            clock.waits.borrow().is_empty(),
            "an observable conflict never waits for consistency"
        );
    }

    #[test]
    fn an_absent_observation_fails_rather_than_remaining_pending() {
        let workspace = Workspace::new("observe-absent");
        workspace.write(
            "observation.yml",
            &format!(
                "$schema: {PUBLICATION_OBSERVATION_SCHEMA}
contract: {PUBLICATION_OBSERVATION_CONTRACT}
release-unit: component
publisher: npm
target: primary
state: absent
"
            ),
        );
        let clock = TestClock::new();
        let error = observe(
            &workspace.root().join("observation.yml"),
            "component/npm/primary",
            &policy(),
            &clock,
        )
        .expect_err("an absent publication is reported");
        assert!(
            error.to_string().contains("is absent from its destination"),
            "{error}"
        );
        assert!(clock.waits.borrow().is_empty());
    }

    // The one retrieval whose bytes the protocol can check for itself is the
    // draft Release asset the release already sealed a sha256 for. A digest in
    // some other spelling makes the join a translation, which is exactly the
    // silence this contract exists to remove.
    #[test]
    fn an_authenticated_draft_retrieval_that_is_not_a_canonical_sha256_is_rejected() {
        let workspace = Workspace::new("observation-draft-digest");
        let document = present_document()
            .replace("publisher: npm", "publisher: homebrew")
            .replace("mode: public", "mode: authenticated-draft");
        workspace.write(
            "observation.yml",
            &document.replacen(
                &format!("digest: {}\n", digest("aa")),
                "digest: sha512-example\n",
                4,
            ),
        );
        let error = PublicationObservation::load(&workspace.root().join("observation.yml"))
            .expect_err("a draft retrieval in a foreign digest form is refused");
        assert!(
            error
                .to_string()
                .contains("authenticated-draft retrieval whose digest"),
            "{error}"
        );

        // The same observation is accepted once it spells the digest the way
        // the draft-asset inventory does.
        workspace.write("observation.yml", &document);
        let observation = PublicationObservation::load(&workspace.root().join("observation.yml"))
            .expect("a canonical draft retrieval digest is accepted");
        assert_eq!(
            observation.retrieval.expect("retrieval").mode,
            CleanClientMode::AuthenticatedDraft
        );
    }

    #[test]
    fn a_present_observation_without_a_retrieval_is_rejected() {
        let workspace = Workspace::new("observe-incomplete");
        workspace.write(
            "observation.yml",
            &present_document().replace(
                &format!(
                    "retrieval:\n  mode: public\n  client: npm\n  version: 10.9.0\n  digest: {}\n",
                    digest("aa")
                ),
                "",
            ),
        );
        let error = PublicationObservation::load(&workspace.root().join("observation.yml"))
            .expect_err("an incomplete present observation is reported");
        assert!(
            error
                .to_string()
                .contains("observes state present without retrieval"),
            "{error}"
        );
    }

    #[test]
    fn a_pending_observation_carrying_destination_detail_is_rejected() {
        let workspace = Workspace::new("observe-overfull");
        workspace.write(
            "observation.yml",
            &format!(
                "$schema: {PUBLICATION_OBSERVATION_SCHEMA}
contract: {PUBLICATION_OBSERVATION_CONTRACT}
release-unit: component
publisher: npm
target: primary
state: pending
destination:
  identity: npmjs
  version: 1.2.3
  digest: {subject}
",
                subject = digest("aa")
            ),
        );
        let error = PublicationObservation::load(&workspace.root().join("observation.yml"))
            .expect_err("a pending observation with destination detail is reported");
        assert!(
            error
                .to_string()
                .contains("observes state pending and carries destination"),
            "{error}"
        );
    }

    #[test]
    fn a_foreign_schema_identity_is_rejected() {
        let workspace = Workspace::new("observe-foreign");
        workspace.write(
            "observation.yml",
            &present_document().replace(PUBLICATION_OBSERVATION_SCHEMA, "https://example.test/v1"),
        );
        let error = PublicationObservation::load(&workspace.root().join("observation.yml"))
            .expect_err("a foreign document is reported");
        assert!(
            error.to_string().contains(PUBLICATION_OBSERVATION_SCHEMA),
            "{error}"
        );
    }

    #[test]
    fn maintained_policies_bound_every_publisher() {
        for publisher in [
            PublisherKind::Npm,
            PublisherKind::Cargo,
            PublisherKind::Homebrew,
            PublisherKind::Rpm,
            PublisherKind::Apt,
            PublisherKind::Aur,
            PublisherKind::Oci,
        ] {
            let policy = ConsistencyPolicy::maintained(publisher);
            assert!(policy.backoff > 1, "{publisher} backs off");
            assert!(
                policy.interval <= policy.maximum_interval
                    && policy.maximum_interval < policy.deadline,
                "{publisher} bounds its observation"
            );
        }
    }
}
