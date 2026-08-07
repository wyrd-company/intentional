// ---
// relationships:
//   implements: github-release-executor
// ---

//! Verification of one destination publication and its affirmative evidence fragment.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::evidence::assemble::{
    AttachedMetadata, CleanClientMode, DestinationAlias, EvidenceReference, PhaseTagEvidence,
    PublisherEvidence, ReleaseIdentity, TagIdentity, PUBLISHER_EVIDENCE_CONTRACT,
    PUBLISHER_EVIDENCE_SCHEMA,
};
use crate::evidence::phase::{decode, PHASE_EVIDENCE_FIELD};
use crate::executor::recipe::{resolve_publications, SelectedPublication, PRIMARY_TARGET};
use crate::model::PublisherKind;
use crate::publication::draft::{
    is_draft_dependent, verify_draft_handoff, DraftReleaseAssetHandoff,
};
use crate::publication::observation::{observe, Clock, ConsistencyPolicy, PublicationObservation};
use crate::publication::release::ReleaseSource;
use crate::release::git;
use crate::release::tag::verify_release_tag;
use std::path::{Path, PathBuf};

/// npm spelling that selects the constrained npm primary registry.
const NPMJS_SELECTOR: &str = "npmjs";
/// Cargo spelling that selects the primary registry only when it is that registry.
const CRATES_IO_SELECTOR: &str = "crates.io";
/// OCI target identities, which an adapter without a primary always requires.
const OCI_TARGETS: [&str; 2] = ["dockerhub", "ghcr"];

/// The release one publication publishes into, and the version it publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRelease {
    /// Release identity resolved from the verified global release tag.
    pub identity: ReleaseIdentity,
    /// Version the reproduced release plan assigns this release unit.
    pub version: String,
}

/// Repository-derived facts one publication verification binds its fragment to.
///
/// The command holds no credentials and speaks no registry protocol, so every
/// fact outside the observation comes from the checkout through this seam.
pub trait PublicationContext {
    /// Release identity and planned version of one release unit.
    ///
    /// Both come from a single resolution, because a version read separately
    /// from the identity it belongs to could describe a different release.
    fn planned_release(&self, root: &Path, release_unit: &str) -> Result<PlannedRelease>;

    /// Fragment an after-publication tag at the release commit already sealed.
    fn sealed_fragment(
        &self,
        root: &Path,
        release_commit: &str,
        identity: &str,
    ) -> Result<Option<PublisherEvidence>>;

    /// Phase tags at the release commit that bind one publication.
    fn phase_tags(
        &self,
        root: &Path,
        release_commit: &str,
        identity: &str,
    ) -> Result<Vec<TagIdentity>>;
}

/// The checkout the publication workflow runs in.
///
/// The release identity, the planned version and the sealed fragments this
/// context reports are proven from the repository rather than read from the
/// observation, so a recipe cannot name the release or version its own evidence
/// claims to belong to.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckoutContext;

impl CheckoutContext {
    /// Read the checkout the publication workflow is running against.
    pub const fn new() -> Self {
        Self
    }
}

/// One annotated tag at the release commit and the phase evidence it sealed.
struct SealedTag {
    identity: TagIdentity,
    evidence: PhaseTagEvidence,
}

/// Every annotated tag at the release commit that sealed phase evidence.
///
/// A tag without phase semantics is skipped rather than reported, because the
/// global release tag and every unphased record legitimately carry none.
fn sealed_tags(root: &Path, release_commit: &str) -> Result<Vec<SealedTag>> {
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let references = repository
        .references()
        .map_err(|error| Error::Git(format!("failed to read references: {error}")))?;
    let tags = references
        .tags()
        .map_err(|error| Error::Git(format!("failed to read tags: {error}")))?;
    let mut sealed = Vec::new();
    for mut reference in tags.flatten() {
        let name = reference.name().shorten().to_string();
        let Some(object_id) = reference.try_id().map(gix::Id::detach) else {
            continue;
        };
        let target = reference
            .peel_to_id()
            .map_err(|error| Error::Git(format!("failed to peel tag {name}: {error}")))?
            .detach();
        if target.to_string() != release_commit {
            continue;
        }
        let object = repository
            .find_object(object_id)
            .map_err(|error| Error::Git(format!("failed to read tag {name}: {error}")))?;
        let Ok(tag) = object.try_into_tag() else {
            continue;
        };
        let decoded = tag
            .decode()
            .map_err(|error| Error::Git(format!("failed to decode tag {name}: {error}")))?;
        let message = decoded.message.to_string();
        let Some(line) = message
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{PHASE_EVIDENCE_FIELD}: ")))
        else {
            continue;
        };
        sealed.push(SealedTag {
            identity: TagIdentity {
                name,
                object: object_id.to_string(),
                target: target.to_string(),
            },
            evidence: decode(line)?,
        });
    }
    sealed.sort_by(|left, right| left.identity.name.cmp(&right.identity.name));
    Ok(sealed)
}

impl PublicationContext for CheckoutContext {
    /// Prove the release from the repository, then take the unit's version from it.
    ///
    /// Reproduction is not cheap: it clones the repository in isolation and
    /// rebuilds the whole candidate from the accepted source commit, and the
    /// publish workflow runs one invocation per selected publication on top of
    /// the dedicated release-tag job, so a release of N publications performs
    /// N+1 reproductions. Repeating it per publication is deliberate: each
    /// publisher job is an independent verifier, and a verifier that inherited
    /// another job's conclusion would be asserting the version rather than
    /// proving it.
    fn planned_release(&self, root: &Path, release_unit: &str) -> Result<PlannedRelease> {
        let verified = verify_release_tag(root)?;
        let version = verified
            .versions
            .get(release_unit)
            .cloned()
            .ok_or_else(|| {
                let planned = if verified.versions.is_empty() {
                    "none".to_owned()
                } else {
                    verified
                        .versions
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                Error::Validation(format!(
                    "release unit {release_unit:?} is not part of release {}; the release publishes {planned}",
                    verified.global_tag
                ))
            })?;
        let reference = format!("refs/tags/{}", verified.global_tag);
        Ok(PlannedRelease {
            identity: ReleaseIdentity {
                source_commit: verified.source,
                release_commit: verified.release,
                global_tag: TagIdentity {
                    object: git::resolve(root, &reference)?,
                    target: git::resolve(root, &format!("{reference}^{{commit}}"))?,
                    name: verified.global_tag,
                },
                plan_digest: verified.plan_digest,
            },
            version,
        })
    }

    fn sealed_fragment(
        &self,
        root: &Path,
        release_commit: &str,
        identity: &str,
    ) -> Result<Option<PublisherEvidence>> {
        let mut found: Option<PublisherEvidence> = None;
        for tag in sealed_tags(root, release_commit)? {
            let Some(sealed) = tag.evidence.publisher_evidence else {
                continue;
            };
            for fragment in sealed {
                if fragment.identity() != identity {
                    continue;
                }
                // Two after-publication tags at the same release commit seal the
                // same publication, so a disagreement between them is a broken
                // record rather than a choice this command may make.
                if found.as_ref().is_some_and(|first| first != &fragment) {
                    return Err(Error::Validation(format!(
                        "after-publication tags at {release_commit} seal conflicting evidence for {identity}"
                    )));
                }
                found = Some(fragment);
            }
        }
        Ok(found)
    }

    fn phase_tags(
        &self,
        root: &Path,
        release_commit: &str,
        identity: &str,
    ) -> Result<Vec<TagIdentity>> {
        let mut bound = Vec::new();
        for tag in sealed_tags(root, release_commit)? {
            let intends = tag
                .evidence
                .intended_destinations
                .iter()
                .flatten()
                .any(|destination| destination.identity() == identity);
            let seals = tag
                .evidence
                .publisher_evidence
                .iter()
                .flatten()
                .any(|fragment| fragment.identity() == identity);
            if intends || seals {
                bound.push(tag.identity);
            }
        }
        Ok(bound)
    }
}

/// Inputs of one `intentional verify publication` invocation.
pub struct VerifyPublicationRequest<'a> {
    /// Workspace whose configuration determines the expected publications.
    pub root: &'a Path,
    /// Release unit whose publication is verified.
    pub release_unit: &'a str,
    /// Package whose publication is verified.
    pub package: &'a str,
    /// Publisher adapter that performed the publication.
    pub publisher: PublisherKind,
    /// Target selector, absent when the adapter's primary is implied.
    pub target: Option<&'a str>,
    /// Observation document the recipe's readback steps wrote.
    pub observation: &'a Path,
    /// File the affirmative fragment is written to.
    pub output: &'a Path,
    /// Observation policy, replacing the adapter's maintained default.
    pub policy: Option<ConsistencyPolicy>,
    /// Observation timing.
    pub clock: &'a dyn Clock,
    /// Repository-derived facts the fragment binds to.
    pub context: &'a dyn PublicationContext,
    /// Draft-Release asset handoff a draft-dependent publisher consumed.
    pub draft_handoff: Option<&'a Path>,
    /// GitHub Release access used to prove that handoff.
    pub release_source: Option<&'a dyn ReleaseSource>,
}

/// One verified publication and the fragment written for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPublication {
    /// Path of the written fragment.
    pub path: PathBuf,
    /// Fragment the assembly job consumes.
    pub evidence: PublisherEvidence,
    /// Whether an after-publication tag had already sealed this fragment.
    pub reused: bool,
}

/// Verify one destination publication and write its affirmative evidence.
pub fn verify_publication(request: &VerifyPublicationRequest<'_>) -> Result<VerifiedPublication> {
    let selected = select_publication(
        request.root,
        request.release_unit,
        request.package,
        request.publisher,
        request.target,
    )?;
    let identity = selected.identity();
    let policy = request.policy.unwrap_or_else(|| {
        let maintained = ConsistencyPolicy::maintained(request.publisher);
        selected
            .observation_deadline
            .map_or(maintained, |deadline| maintained.with_deadline(deadline))
    });
    let observation = observe(request.observation, &identity, &policy, request.clock)?;
    let release = request
        .context
        .planned_release(request.root, &selected.release_unit)?;
    let observed = accept_observation(&observation, &selected, &identity, &release.version)?;
    join_draft_retrieval(request, &selected, &identity, observed.retrieval)?;
    let identity_facts = &release.identity;

    let sealed =
        request
            .context
            .sealed_fragment(request.root, &identity_facts.release_commit, &identity)?;
    let (evidence, reused) = match sealed {
        Some(fragment) => {
            revalidate_sealed(&fragment, identity_facts, &observed, &identity)?;
            (fragment, true)
        }
        None => {
            let phase_tags = request.context.phase_tags(
                request.root,
                &identity_facts.release_commit,
                &identity,
            )?;
            (
                construct(
                    &selected,
                    &observation,
                    &observed,
                    identity_facts,
                    phase_tags,
                ),
                false,
            )
        }
    };

    let document = serde_yaml::to_string(&evidence)?;
    if let Some(parent) = request.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        }
    }
    std::fs::write(request.output, document).map_err(|error| Error::io(request.output, error))?;
    Ok(VerifiedPublication {
        path: request.output.to_path_buf(),
        evidence,
        reused,
    })
}

/// Bind a draft-dependent publisher's retrieval to the assets the release sealed.
///
/// The retrieval a draft-dependent recipe records and the draft-asset inventory
/// the release sealed are the two sides of one claim, and until they are
/// compared each is a separate assertion about the same bytes. The recipe says
/// it downloaded a draft asset and produced a digest; the handoff says which
/// assets belong to this publication and what their canonical sha256 values are.
/// Joining them is what makes the mode more than a word: the digest the fragment
/// carries has to be a digest [`retrieve_assets`] proved by downloading the
/// asset and hashing exactly the bytes it received.
///
/// The join is required rather than opportunistic. A draft-dependent publisher
/// that verified without a handoff would record authenticated-draft retrieval
/// with nothing on the other side of the comparison, which is the same silence
/// the mode exists to remove.
fn join_draft_retrieval(
    request: &VerifyPublicationRequest<'_>,
    selected: &SelectedPublication,
    identity: &str,
    retrieval: &crate::evidence::assemble::CleanClient,
) -> Result<()> {
    if !is_draft_dependent(selected.publisher) {
        // A publisher whose consumer path never resolves a Release asset has no
        // inventory to be compared against, and supplying one would assert a
        // handoff the release never made.
        if request.draft_handoff.is_some() {
            return Err(Error::Validation(format!(
                "publication {identity} supplies a draft-Release asset handoff; the {} publisher resolves its release through the public consumer path and consumes no draft asset",
                selected.publisher
            )));
        }
        return Ok(());
    }
    let (Some(path), Some(source)) = (request.draft_handoff, request.release_source) else {
        return Err(Error::Validation(format!(
            "publication {identity} records authenticated draft-asset retrieval, which is proved against the draft-Release asset handoff the release sealed; supply that handoff document"
        )));
    };
    let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
    let handoff = DraftReleaseAssetHandoff::from_yaml(&text)?;
    if handoff.identity() != identity {
        return Err(Error::Validation(format!(
            "the draft-Release asset handoff serves publication {} instead of {identity}",
            handoff.identity()
        )));
    }
    let verified = verify_draft_handoff(request.root, &handoff, source)?;
    if !verified
        .retrieval
        .assets()
        .iter()
        .any(|asset| asset.sha256 == retrieval.digest)
    {
        return Err(Error::Validation(format!(
            "publication {identity} records retrieving {}, which is not the digest of any asset the draft-Release handoff inventories; the release sealed {}",
            retrieval.digest,
            verified
                .retrieval
                .assets()
                .iter()
                .map(|asset| format!("{} at {}", asset.name, asset.sha256))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}

/// Members a present observation always carries, borrowed together.
struct ObservedPublication<'a> {
    subject: &'a crate::evidence::assemble::Subject,
    packager: &'a crate::evidence::assemble::PackagerRecord,
    destination: &'a crate::evidence::assemble::Destination,
    retrieval: &'a crate::evidence::assemble::CleanClient,
}

/// Resolve one selector to the single configured publication it names.
fn select_publication(
    root: &Path,
    release_unit: &str,
    package: &str,
    publisher: PublisherKind,
    selector: Option<&str>,
) -> Result<SelectedPublication> {
    let config = Config::load(root)?;
    let selection = resolve_publications(root, &config)?;
    let configured: Vec<&SelectedPublication> = selection
        .selected
        .iter()
        .filter(|publication| {
            publication.release_unit == release_unit
                && publication.package == package
                && publication.publisher == publisher
        })
        .collect();
    let target = canonical_target(publisher, selector, &configured)?;
    configured
        .into_iter()
        .find(|publication| publication.target == target)
        .cloned()
        .ok_or_else(|| {
            let expected = if selection.selected.is_empty() {
                "none".to_owned()
            } else {
                selection
                    .selected
                    .iter()
                    .map(SelectedPublication::identity)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            Error::Validation(format!(
                "publication {release_unit}/{package}/{publisher}/{target} is not configured; configured publications are {expected}"
            ))
        })
}

/// Normalize one command-line target selector to a canonical target identity.
///
/// Selector spellings exist so an operator names a destination the way its
/// ecosystem does. They resolve to canonical identities here and never reach
/// evidence, where one identity per adapter keeps fragments comparable.
fn canonical_target(
    publisher: PublisherKind,
    selector: Option<&str>,
    configured: &[&SelectedPublication],
) -> Result<String> {
    if publisher == PublisherKind::Oci {
        return match selector {
            Some(target) if OCI_TARGETS.contains(&target) => Ok(target.to_owned()),
            Some(PRIMARY_TARGET) | None => Err(Error::Validation(format!(
                "the oci publisher has no primary target; select --target {}",
                OCI_TARGETS.join(" or --target ")
            ))),
            Some(target) => Err(Error::Validation(format!(
                "target {target:?} is not an oci target; the oci publisher accepts {}",
                OCI_TARGETS.join(" or ")
            ))),
        };
    }
    match selector {
        None | Some(PRIMARY_TARGET) => Ok(PRIMARY_TARGET.to_owned()),
        Some(NPMJS_SELECTOR) if publisher == PublisherKind::Npm => Ok(PRIMARY_TARGET.to_owned()),
        Some(CRATES_IO_SELECTOR) if publisher == PublisherKind::Cargo => {
            let primary = configured
                .iter()
                .find(|publication| publication.target == PRIMARY_TARGET);
            match primary.and_then(|publication| publication.destination.as_deref()) {
                Some(CRATES_IO_SELECTOR) | None => Ok(PRIMARY_TARGET.to_owned()),
                Some(registry) => Err(Error::Validation(format!(
                    "target {CRATES_IO_SELECTOR:?} does not name the configured Cargo primary registry {registry:?}; select the configured primary with --target {PRIMARY_TARGET}"
                ))),
            }
        }
        Some(target) => Ok(target.to_owned()),
    }
}

/// Consumer path one retrieval mode names, for a diagnostic that says why.
const fn retrieval_path(mode: CleanClientMode) -> &'static str {
    match mode {
        CleanClientMode::Public => "the public consumer path",
        CleanClientMode::AuthenticatedDraft => "a draft GitHub Release asset",
        CleanClientMode::AuthenticatedRegistry => {
            "its normal client path, which admits no anonymous read"
        }
    }
}

/// Bind one present observation to the publication it claims to describe.
fn accept_observation<'a>(
    observation: &'a PublicationObservation,
    selected: &SelectedPublication,
    identity: &str,
    version: &str,
) -> Result<ObservedPublication<'a>> {
    let observed = observation.identity();
    if observed != identity {
        return Err(Error::Validation(format!(
            "the observation describes publication {observed} instead of {identity}"
        )));
    }
    let (Some(subject), Some(packager), Some(destination), Some(retrieval)) = (
        observation.subject.as_ref(),
        observation.packager.as_ref(),
        observation.destination.as_ref(),
        observation.retrieval.as_ref(),
    ) else {
        return Err(Error::Validation(format!(
            "the observation of {identity} is not a complete present readback"
        )));
    };
    if packager.id != selected.packager.as_str() {
        return Err(Error::Validation(format!(
            "publication {identity} was observed with packager {:?} instead of configured packager {:?}",
            packager.id,
            selected.packager.as_str()
        )));
    }
    if let Some(configured) = selected.destination.as_deref() {
        if destination.identity != configured {
            return Err(Error::Validation(format!(
                "publication {identity} was observed at destination {:?} instead of its configured destination {configured:?}",
                destination.identity
            )));
        }
    }
    // An observation left at the conventional path by an earlier release of the
    // same publication is identical in identity and destination, so the version
    // this release plans is the only thing that separates them.
    //
    // Both versions are compared verbatim, which is the contract the
    // publication-observation specification states: a recipe reports the plan's
    // spelling here and records an ecosystem's own spelling of the same release
    // as a destination alias, so reporting the destination faithfully never
    // costs it the binding check.
    for (claim, observed) in [
        ("subject version", &subject.version),
        ("destination version", &destination.version),
    ] {
        if observed != version {
            return Err(Error::Validation(format!(
                "publication {identity} was observed with {claim} {observed:?} instead of {version:?}, the version this release publishes for release unit {}",
                selected.release_unit
            )));
        }
    }
    // The recipe fixes consumer retrieval, so the mode a destination admits is
    // the recipe's and not the observation's to choose. Verification runs before
    // closure, so a draft-dependent publisher claiming public retrieval claims
    // something that cannot be true yet; a destination that serves no anonymous
    // client cannot be retrieved publicly at all; and a public destination
    // claiming either authenticated mode claims a check it did not perform.
    if retrieval.mode != selected.retrieval {
        return Err(Error::Validation(format!(
            "publication {identity} claims {} retrieval; the maintained recipe for this destination retrieves it through {} and records mode {}",
            retrieval.mode.as_str(),
            retrieval_path(selected.retrieval),
            selected.retrieval.as_str()
        )));
    }
    Ok(ObservedPublication {
        subject,
        packager,
        destination,
        retrieval,
    })
}

/// Build the affirmative fragment from one accepted observation.
fn construct(
    selected: &SelectedPublication,
    observation: &PublicationObservation,
    observed: &ObservedPublication<'_>,
    release: &ReleaseIdentity,
    phase_tags: Vec<TagIdentity>,
) -> PublisherEvidence {
    let mut build_provenance = observation.build_provenance.clone();
    build_provenance.sort_by_key(reference_key);
    let mut attached_metadata = observation.attached_metadata.clone();
    attached_metadata.sort_by_key(metadata_key);
    let mut destination_aliases = observation.destination_aliases.clone();
    destination_aliases.sort_by_key(alias_key);
    let mut phase_tags = phase_tags;
    phase_tags.sort_by_key(|tag| (tag.name.clone(), tag.object.clone()));
    PublisherEvidence {
        schema: PUBLISHER_EVIDENCE_SCHEMA.to_owned(),
        contract: PUBLISHER_EVIDENCE_CONTRACT.to_owned(),
        release_unit: selected.release_unit.clone(),
        package: selected.package.clone(),
        publisher: selected.publisher,
        target: selected.target.clone(),
        source_commit: release.source_commit.clone(),
        release_commit: release.release_commit.clone(),
        global_tag: release.global_tag.clone(),
        plan_digest: release.plan_digest.clone(),
        subject: observed.subject.clone(),
        packager: observed.packager.clone(),
        build_provenance,
        attached_metadata,
        destination: observed.destination.clone(),
        clean_client: observed.retrieval.clone(),
        destination_aliases,
        phase_tags,
    }
}

/// Stable ordering key of one supply-chain reference.
fn reference_key(reference: &EvidenceReference) -> (String, String, String) {
    (
        reference.kind.clone(),
        reference.digest.clone(),
        reference.reference.clone().unwrap_or_default(),
    )
}

/// Stable ordering key of one attached component.
fn metadata_key(metadata: &AttachedMetadata) -> (String, String, String) {
    (
        metadata.kind.to_string(),
        metadata.digest.clone(),
        metadata.reference.clone().unwrap_or_default(),
    )
}

/// Stable ordering key of one mutable destination alias.
fn alias_key(alias: &DestinationAlias) -> (String, String) {
    (alias.name.clone(), alias.digest.clone())
}

/// Revalidate only the immutable claims of an already sealed fragment.
///
/// A retry runs on a different runner, after aliases have advanced and with
/// different tool versions, so comparing mutable state would reject the very
/// historical claim the sealed tag exists to preserve.
fn revalidate_sealed(
    fragment: &PublisherEvidence,
    release: &ReleaseIdentity,
    observed: &ObservedPublication<'_>,
    identity: &str,
) -> Result<()> {
    let sealed_identity = fragment.identity();
    if sealed_identity != identity {
        return Err(Error::Validation(format!(
            "the after-publication tag seals publication {sealed_identity} instead of {identity}"
        )));
    }
    for (claim, sealed, current) in [
        (
            "source commit",
            &fragment.source_commit,
            &release.source_commit,
        ),
        (
            "release commit",
            &fragment.release_commit,
            &release.release_commit,
        ),
        (
            "global tag name",
            &fragment.global_tag.name,
            &release.global_tag.name,
        ),
        (
            "global tag object",
            &fragment.global_tag.object,
            &release.global_tag.object,
        ),
        (
            "global tag target",
            &fragment.global_tag.target,
            &release.global_tag.target,
        ),
        ("plan digest", &fragment.plan_digest, &release.plan_digest),
        (
            "subject digest",
            &fragment.subject.digest,
            &observed.subject.digest,
        ),
        (
            "destination identity",
            &fragment.destination.identity,
            &observed.destination.identity,
        ),
        (
            "destination version",
            &fragment.destination.version,
            &observed.destination.version,
        ),
    ] {
        if sealed != current {
            return Err(Error::Validation(format!(
                "the sealed fragment for {identity} records {claim} {sealed:?} while the release states {current:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::assemble::{
        CleanClient, Destination, PackagerRecord, PhaseTagEvidence, Subject,
    };
    use crate::executor::fixture::Workspace;
    use crate::publication::observation::tests::{digest, present_document};
    use crate::publication::observation::{
        ObservationState, PUBLICATION_OBSERVATION_CONTRACT, PUBLICATION_OBSERVATION_SCHEMA,
    };
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::time::Duration;

    /// Observation timing that never advances, because these tests never wait.
    struct StillClock(Cell<Duration>);

    impl Clock for StillClock {
        fn elapsed(&self) -> Duration {
            self.0.get()
        }

        fn wait(&self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }

    /// Repository facts supplied directly by a test.
    struct TestContext {
        release: ReleaseIdentity,
        version: String,
        sealed: BTreeMap<String, PublisherEvidence>,
        phase_tags: Vec<TagIdentity>,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                version: "1.2.3".to_owned(),
                release: ReleaseIdentity {
                    source_commit: "a".repeat(40),
                    release_commit: "b".repeat(40),
                    global_tag: TagIdentity {
                        name: "release/1.2.3".to_owned(),
                        object: "c".repeat(40),
                        target: "b".repeat(40),
                    },
                    plan_digest: digest("ee"),
                },
                sealed: BTreeMap::new(),
                phase_tags: Vec::new(),
            }
        }
    }

    impl PublicationContext for TestContext {
        fn planned_release(&self, _root: &Path, _release_unit: &str) -> Result<PlannedRelease> {
            Ok(PlannedRelease {
                identity: self.release.clone(),
                version: self.version.clone(),
            })
        }

        fn sealed_fragment(
            &self,
            _root: &Path,
            _release_commit: &str,
            identity: &str,
        ) -> Result<Option<PublisherEvidence>> {
            Ok(self.sealed.get(identity).cloned())
        }

        fn phase_tags(
            &self,
            _root: &Path,
            _release_commit: &str,
            _identity: &str,
        ) -> Result<Vec<TagIdentity>> {
            Ok(self.phase_tags.clone())
        }
    }

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#;

    /// A workspace whose sole release unit declares one publisher block.
    fn workspace(label: &str, publisher: &str, files: &[(&str, &str)]) -> Workspace {
        let package = publisher
            .lines()
            .map(|line| format!("    {line}\n"))
            .collect::<String>();
        let workspace = Workspace::new(label);
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace(
                "    path: component\n",
                &format!(
                    "    path: component\n    packages:\n      package:\n        path: .\n{package}"
                ),
            ),
        );
        for (path, contents) in files {
            workspace.write(path, contents);
        }
        workspace
    }

    /// A workspace publishing the sample npm library through its primary.
    fn npm_workspace(label: &str) -> Workspace {
        workspace(
            label,
            "    npm: { npmjs: {} }\n",
            &[(
                "component/package.json",
                r#"{"name":"sample-library","version":"1.2.3"}"#,
            )],
        )
    }

    fn request<'a>(
        workspace: &'a Workspace,
        publisher: PublisherKind,
        target: Option<&'a str>,
        observation: &'a Path,
        output: &'a Path,
        clock: &'a StillClock,
        context: &'a dyn PublicationContext,
    ) -> VerifyPublicationRequest<'a> {
        VerifyPublicationRequest {
            root: workspace.root(),
            release_unit: "component",
            package: "package",
            publisher,
            target,
            observation,
            output,
            policy: Some(ConsistencyPolicy {
                interval: Duration::from_secs(1),
                backoff: 2,
                maximum_interval: Duration::from_secs(4),
                deadline: Duration::from_secs(4),
            }),
            clock,
            context,
            draft_handoff: None,
            release_source: None,
        }
    }

    fn clock() -> StillClock {
        StillClock(Cell::new(Duration::ZERO))
    }

    #[test]
    fn refuses_a_publisher_configured_only_for_another_package() {
        let workspace = Workspace::new("verify-package-selector");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG.replace(
                "    path: component\n",
                "    path: component\n    packages:\n      first:\n        path: first\n        npm: { npmjs: {} }\n      second:\n        path: second\n        cargo: { registry: {} }\n",
            ),
        );
        workspace.write(
            "component/first/package.json",
            r#"{"name":"sample-library","version":"1.2.3"}"#,
        );
        workspace.write(
            "component/second/Cargo.toml",
            "[package]\nname = \"sample-crate\"\nversion = \"1.2.3\"\n",
        );

        let error = select_publication(
            workspace.root(),
            "component",
            "second",
            PublisherKind::Npm,
            None,
        )
        .expect_err("a publisher from another package must not satisfy the selector");

        assert!(
            error
                .to_string()
                .contains("publication component/second/npm/primary is not configured"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_present_observation_produces_a_deterministic_fragment() {
        let workspace = npm_workspace("verify-present");
        workspace.write(
            "observation.yml",
            &present_document().replace(
                "state: present\n",
                &format!(
                    "state: present
build-provenance:
  - kind: sbom
    digest: {second}
  - kind: attestation
    digest: {first}
    reference: https://example.test/attestation
destination-aliases:
  - name: next
    digest: {second}
  - name: latest
    digest: {first}
",
                    first = digest("11"),
                    second = digest("22")
                ),
            ),
        );
        let context = TestContext {
            phase_tags: vec![
                TagIdentity {
                    name: "component@1.2.3-published".to_owned(),
                    object: "d".repeat(40),
                    target: "b".repeat(40),
                },
                TagIdentity {
                    name: "component@1.2.3-building".to_owned(),
                    object: "e".repeat(40),
                    target: "b".repeat(40),
                },
            ],
            ..TestContext::new()
        };
        let clock = clock();
        let observation = workspace.root().join("observation.yml");
        let first = workspace.root().join("first.yml");
        let second = workspace.root().join("second.yml");
        let verified = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &observation,
            &first,
            &clock,
            &context,
        ))
        .expect("the publication verifies");
        assert!(!verified.reused);
        assert_eq!(
            verified.evidence.identity(),
            "component/package/npm/primary"
        );
        assert_eq!(verified.evidence.target, PRIMARY_TARGET);
        assert_eq!(verified.evidence.destination.identity, "npmjs");
        assert_eq!(verified.evidence.schema, PUBLISHER_EVIDENCE_SCHEMA);
        assert_eq!(verified.evidence.contract, PUBLISHER_EVIDENCE_CONTRACT);
        assert_eq!(
            verified
                .evidence
                .build_provenance
                .iter()
                .map(|reference| reference.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["attestation", "sbom"],
            "references are ordered by a stable key"
        );
        assert_eq!(
            verified
                .evidence
                .destination_aliases
                .iter()
                .map(|alias| alias.name.as_str())
                .collect::<Vec<_>>(),
            vec!["latest", "next"]
        );
        assert_eq!(
            verified
                .evidence
                .phase_tags
                .iter()
                .map(|tag| tag.name.as_str())
                .collect::<Vec<_>>(),
            vec!["component@1.2.3-building", "component@1.2.3-published"]
        );

        verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &observation,
            &second,
            &clock,
            &context,
        ))
        .expect("the publication verifies again");
        assert_eq!(
            std::fs::read_to_string(&first).expect("first fragment"),
            std::fs::read_to_string(&second).expect("second fragment"),
            "two runs over one observation are byte-identical"
        );
    }

    #[test]
    fn an_observation_from_a_neighbouring_packager_is_rejected() {
        let workspace = npm_workspace("verify-packager-binding");
        workspace.write(
            "observation.yml",
            &present_document().replace("  id: npm", "  id: cargo"),
        );
        let observation = workspace.root().join("observation.yml");
        let output = workspace.root().join("evidence.yml");
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &observation,
            &output,
            &clock(),
            &TestContext::new(),
        ))
        .expect_err("an observation from another configured packager is refused");

        assert!(
            error.to_string().contains(
                "observed with packager \"cargo\" instead of configured packager \"npm\""
            ),
            "{error}"
        );
        assert!(!output.exists(), "a refused observation writes no evidence");
    }

    #[test]
    fn the_npmjs_selector_names_the_npm_primary() {
        let workspace = npm_workspace("verify-npmjs");
        workspace.write("observation.yml", &present_document());
        let context = TestContext::new();
        let clock = clock();
        let verified = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            Some(NPMJS_SELECTOR),
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect("the npmjs alias selects the primary");
        assert_eq!(
            verified.evidence.target, PRIMARY_TARGET,
            "a selector alias never reaches evidence"
        );
    }

    #[test]
    fn the_crates_io_selector_names_only_a_crates_io_primary() {
        let files = [(
            "component/Cargo.toml",
            "[package]\nname = \"sample-library\"\nversion = \"1.2.3\"\n",
        )];
        let accepted = workspace("verify-crates-io", "    cargo: { registry: {} }\n", &files);
        accepted.write(
            "observation.yml",
            &present_document()
                .replace("publisher: npm", "publisher: cargo")
                .replace("identity: npmjs", "identity: crates.io")
                .replace("kind: npm-package", "kind: crate")
                .replace("id: npm", "id: cargo")
                .replace("client: npm", "client: cargo"),
        );
        let context = TestContext::new();
        let clock = clock();
        let verified = verify_publication(&request(
            &accepted,
            PublisherKind::Cargo,
            Some(CRATES_IO_SELECTOR),
            &accepted.root().join("observation.yml"),
            &accepted.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect("the crates.io alias selects the crates.io primary");
        assert_eq!(verified.evidence.target, PRIMARY_TARGET);
        assert_eq!(verified.evidence.destination.identity, "crates.io");

        let custom = workspace(
            "verify-custom-registry",
            "    cargo: { registry: {} }\n",
            &[(
                "component/Cargo.toml",
                "[package]\nname = \"sample-library\"\nversion = \"1.2.3\"\npublish = [\"example-registry\"]\n",
            )],
        );
        custom.write("observation.yml", &present_document());
        let error = verify_publication(&request(
            &custom,
            PublisherKind::Cargo,
            Some(CRATES_IO_SELECTOR),
            &custom.root().join("observation.yml"),
            &custom.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("the alias never redirects to another registry");
        assert!(
            error.to_string().contains("example-registry")
                && error.to_string().contains("--target primary"),
            "{error}"
        );
    }

    #[test]
    fn the_oci_publisher_requires_an_explicit_target() {
        let workspace = workspace(
            "verify-oci",
            "    oci:\n      ghcr: {}\n",
            &[("component/Dockerfile", "FROM scratch\n")],
        );
        workspace.write(
            "observation.yml",
            &present_document()
                .replace("publisher: npm", "publisher: oci")
                .replace("target: primary", "target: ghcr")
                .replace("identity: npmjs", "identity: ghcr.io/example-org/component")
                .replace("kind: npm-package", "kind: oci-image")
                .replace(
                    "id: npm\n  version: 10.9.0",
                    "id: buildx\n  version: 0.17.0",
                )
                .replace("client: npm", "client: crane"),
        );
        let context = TestContext::new();
        let clock = clock();
        for selector in [None, Some(PRIMARY_TARGET)] {
            let error = verify_publication(&request(
                &workspace,
                PublisherKind::Oci,
                selector,
                &workspace.root().join("observation.yml"),
                &workspace.root().join("evidence.yml"),
                &clock,
                &context,
            ))
            .expect_err("an oci publication names its registry");
            assert!(
                error.to_string().contains("has no primary target"),
                "{error}"
            );
        }
        let verified = verify_publication(&request(
            &workspace,
            PublisherKind::Oci,
            Some("ghcr"),
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect("an explicit oci target verifies");
        assert_eq!(verified.evidence.target, "ghcr");
    }

    #[test]
    fn an_unconfigured_publication_names_the_configured_ones() {
        let workspace = npm_workspace("verify-unconfigured");
        workspace.write("observation.yml", &present_document());
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            Some("github"),
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("an unconfigured target is reported");
        assert!(
            error
                .to_string()
                .contains("publication component/package/npm/github is not configured")
                && error.to_string().contains("component/package/npm/primary"),
            "{error}"
        );
        assert!(
            !workspace.root().join("evidence.yml").exists(),
            "a refused verification writes nothing"
        );
    }

    #[test]
    fn a_draft_mode_claim_from_a_public_publisher_is_rejected() {
        let workspace = npm_workspace("verify-draft-claim");
        workspace.write(
            "observation.yml",
            &present_document().replace("mode: public", "mode: authenticated-draft"),
        );
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a draft-mode claim from npm is reported");
        assert!(error.to_string().contains("authenticated-draft"), "{error}");
        assert!(!workspace.root().join("evidence.yml").exists());
    }

    /// A workspace publishing the sample component through a draft-dependent tap.
    fn homebrew_workspace(label: &str, mode: &str) -> Workspace {
        let workspace = workspace(
            label,
            "    homebrew:\n      repository: example-org/example-tap\n",
            &[
                ("component/go.mod", "module example.test/component\n"),
                ("component/main.go", "package main\n\nfunc main() {}\n"),
            ],
        );
        workspace.write(
            "observation.yml",
            &present_document()
                .replace("publisher: npm", "publisher: homebrew")
                .replace("identity: npmjs", "identity: example-org/example-tap")
                .replace("kind: npm-package", "kind: homebrew-formula")
                .replace(
                    "id: npm\n  version: 10.9.0",
                    "id: goreleaser\n  version: 2.4.0",
                )
                .replace("mode: public", &format!("mode: {mode}"))
                .replace("client: npm", "client: brew"),
        );
        workspace
    }

    /// Bytes the released fixture's draft Release carries as its one asset.
    const DELIVERABLE: &[u8] = b"deliverable bytes";

    /// One released workspace, its handoff document, and the draft serving it.
    ///
    /// The join is a comparison against a draft the release actually sealed, so
    /// its fixture is a real released repository rather than a directory of
    /// configuration. Anything less would prove the comparison compiles.
    struct DraftFixture {
        released: crate::publication::draft::tests::ReleasedWorkspace,
        source: crate::publication::release::tests::FakeReleaseSource,
        handoff: PathBuf,
        observation: PathBuf,
        output: PathBuf,
    }

    impl DraftFixture {
        fn new(retrieved: &str) -> Self {
            let released = crate::publication::draft::tests::ReleasedWorkspace::new();
            let handoff = released.root.join("handoff.yml");
            std::fs::write(
                &handoff,
                released
                    .handoff(DELIVERABLE)
                    .to_yaml()
                    .expect("handoff renders"),
            )
            .expect("handoff written");
            let observation = released.root.join("observation.yml");
            std::fs::write(
                &observation,
                present_document()
                    .replace("publisher: npm", "publisher: homebrew")
                    .replace("identity: npmjs", "identity: example-owner/homebrew-example")
                    .replace("kind: npm-package", "kind: homebrew-formula")
                    .replace("id: npm\n  version: 10.9.0", "id: goreleaser\n  version: 2.4.0")
                    .replace("mode: public", "mode: authenticated-draft")
                    .replace("client: npm", "client: brew")
                    .replace(
                        &format!(
                            "retrieval:\n  mode: authenticated-draft\n  client: brew\n  version: 10.9.0\n  digest: {}\n",
                            digest("aa")
                        ),
                        &format!(
                            "retrieval:\n  mode: authenticated-draft\n  client: brew\n  version: 10.9.0\n  digest: {retrieved}\n"
                        ),
                    ),
            )
            .expect("observation written");
            Self {
                source: released.source(DELIVERABLE),
                output: released.root.join("evidence.yml"),
                released,
                handoff,
                observation,
            }
        }

        fn verify<'a>(
            &'a self,
            clock: &'a StillClock,
            context: &'a dyn PublicationContext,
        ) -> Result<VerifiedPublication> {
            verify_publication(&VerifyPublicationRequest {
                root: &self.released.root,
                release_unit: "component",
                package: "package",
                publisher: PublisherKind::Homebrew,
                target: None,
                observation: &self.observation,
                output: &self.output,
                policy: Some(ConsistencyPolicy {
                    interval: Duration::from_secs(1),
                    backoff: 2,
                    maximum_interval: Duration::from_secs(4),
                    deadline: Duration::from_secs(4),
                }),
                clock,
                context,
                draft_handoff: Some(&self.handoff),
                release_source: Some(&self.source),
            })
        }
    }

    // The recipe's claim and the release's inventory are two assertions about
    // the same bytes until they are compared. This is that comparison.
    #[test]
    fn binds_an_authenticated_draft_retrieval_to_the_sealed_asset_inventory() {
        let fixture = DraftFixture::new(&crate::evidence::digest_bytes(DELIVERABLE));
        let clock = clock();
        let context = TestContext::new();
        let verified = fixture
            .verify(&clock, &context)
            .expect("a draft-dependent publication verifies against its handoff");
        assert_eq!(
            verified.evidence.clean_client.mode,
            CleanClientMode::AuthenticatedDraft,
            "the recorded mode is exactly the one observed"
        );
    }

    #[test]
    fn rejects_a_draft_retrieval_digest_no_inventoried_asset_carries() {
        // The digest a defect produces is a plausible one: the bytes of some
        // other artifact this release also built, not a nonsense value.
        let fixture = DraftFixture::new(&crate::evidence::digest_bytes(b"a different artifact"));
        let clock = clock();
        let context = TestContext::new();
        let error = fixture
            .verify(&clock, &context)
            .expect_err("a digest outside the inventory is reported");
        assert!(
            error
                .to_string()
                .contains("which is not the digest of any asset"),
            "{error}"
        );
        assert!(error.to_string().contains("component-1.0.0.tgz"), "{error}");
    }

    #[test]
    fn requires_a_handoff_before_recording_authenticated_draft_retrieval() {
        let workspace = homebrew_workspace("verify-draft-unproved", "authenticated-draft");
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Homebrew,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("an unproved draft claim is reported");
        assert!(
            error.to_string().contains("supply that handoff document"),
            "{error}"
        );
        assert!(!workspace.root().join("evidence.yml").exists());
    }

    #[test]
    fn refuses_a_handoff_from_a_publisher_that_consumes_no_draft_asset() {
        let workspace = npm_workspace("verify-handoff-unexpected");
        workspace.write("observation.yml", &present_document());
        let handoff = workspace.root().join("handoff.yml");
        std::fs::write(&handoff, "unused").expect("handoff written");
        let context = TestContext::new();
        let clock = clock();
        let observation = workspace.root().join("observation.yml");
        let output = workspace.root().join("evidence.yml");
        let mut request = request(
            &workspace,
            PublisherKind::Npm,
            None,
            &observation,
            &output,
            &clock,
            &context,
        );
        request.draft_handoff = Some(&handoff);
        let error = verify_publication(&request).expect_err("an unexpected handoff is reported");
        assert!(
            error.to_string().contains("consumes no draft asset"),
            "{error}"
        );
    }

    // GitHub Package Registry serves no anonymous client, so its recipe fixes
    // authenticated-registry retrieval while the same npm publisher's npmjs
    // primary fixes public. A check keyed on the publisher would accept either
    // claim at either destination, which is how a destination ends up with
    // affirmative evidence of a retrieval nobody could have performed.
    #[test]
    fn each_npm_destination_records_the_retrieval_its_own_recipe_fixes() {
        let workspace = workspace(
            "verify-npm-github-retrieval",
            "    npm: { npmjs: {}, github: {} }\n",
            &[(
                "component/package.json",
                r#"{"name":"sample-library","version":"1.2.3"}"#,
            )],
        );
        let context = TestContext::new();
        let clock = clock();

        workspace.write(
            "observation.yml",
            &present_document()
                .replace("target: primary", "target: github")
                .replace("identity: npmjs", "identity: npm.pkg.github.com")
                .replace("mode: public", "mode: authenticated-registry"),
        );
        let verified = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            Some("github"),
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect("the GitHub Package Registry target verifies");
        assert_eq!(
            verified.evidence.clean_client.mode,
            CleanClientMode::AuthenticatedRegistry
        );

        workspace.write(
            "observation.yml",
            &present_document()
                .replace("target: primary", "target: github")
                .replace("identity: npmjs", "identity: npm.pkg.github.com"),
        );
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            Some("github"),
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a public claim from a registry that serves no anonymous client is reported");
        assert!(
            error.to_string().contains("claims public retrieval")
                && error
                    .to_string()
                    .contains("records mode authenticated-registry"),
            "{error}"
        );

        workspace.write(
            "observation.yml",
            &present_document().replace("mode: public", "mode: authenticated-registry"),
        );
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("an authenticated-registry claim from the public npmjs primary is reported");
        assert!(
            error
                .to_string()
                .contains("claims authenticated-registry retrieval")
                && error.to_string().contains("records mode public"),
            "{error}"
        );
    }

    #[test]
    fn a_public_mode_claim_from_a_draft_dependent_publisher_is_rejected() {
        let workspace = homebrew_workspace("verify-public-claim", "public");
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Homebrew,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a public claim from a draft-dependent publisher is reported");
        assert!(
            error.to_string().contains("claims public retrieval")
                && error.to_string().contains("homebrew"),
            "{error}"
        );
        assert!(!workspace.root().join("evidence.yml").exists());
    }

    #[test]
    fn an_observation_whose_subject_version_is_another_releases_is_rejected() {
        let workspace = npm_workspace("verify-foreign-subject-version");
        workspace.write(
            "observation.yml",
            &present_document().replacen("version: 1.2.3", "version: 1.2.2", 1),
        );
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a subject from another release is reported");
        assert!(
            error.to_string().contains("subject version \"1.2.2\"")
                && error.to_string().contains("\"1.2.3\""),
            "{error}"
        );
        assert!(
            !workspace.root().join("evidence.yml").exists(),
            "a refused verification writes nothing"
        );
    }

    #[test]
    fn an_observation_whose_destination_version_disagrees_is_rejected() {
        let workspace = npm_workspace("verify-foreign-destination-version");
        workspace.write(
            "observation.yml",
            &present_document().replace(
                "destination:\n  identity: npmjs\n  version: 1.2.3",
                "destination:\n  identity: npmjs\n  version: 1.2.2",
            ),
        );
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a destination version from another release is reported");
        assert!(
            error.to_string().contains("destination version \"1.2.2\"")
                && error.to_string().contains("\"1.2.3\""),
            "{error}"
        );
    }

    /// A workspace whose sole release unit projects its version and publishes it.
    const RELEASED_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
workspace-tags:
  release:
    template: 'release/{version}'
release-units:
  component:
    path: component
    packages:
      package:
        path: .
        npm: { npmjs: {} }
    projections:
      - adapter: json
        file: package.json
        pointer: /version
        mode: committed
    tags:
      primary:
        role: primary
        template: '{id}@{version}'
        require-phase: after-publication
"#;

    /// Version the fixture's first release recorded for its release unit.
    const PREVIOUS_VERSION: &str = "1.0.0";
    /// Version the fixture's second release plans for the same release unit.
    const CURRENT_VERSION: &str = "1.1.0";

    /// A repository carrying a released commit over an earlier tagged release.
    ///
    /// The version binding is only observable when the two releases of the same
    /// publication disagree, so the fixture tags a first version and then
    /// releases a second one over it.
    struct ReleasedWorkspace {
        workspace: Workspace,
    }

    /// Run one git command in the fixture repository.
    fn git(root: &Path, arguments: &[&str]) -> String {
        crate::release::git::GitCommand::new(root)
            .args(arguments)
            .run()
            .unwrap_or_else(|error| panic!("git {arguments:?} failed: {error}"))
            .line()
            .expect("git output")
    }

    impl ReleasedWorkspace {
        /// Tag a first release, then build and publish a second one over it.
        fn new(label: &str) -> Self {
            let workspace = Workspace::new(label);
            workspace
                .write(".intentional/config.yml", RELEASED_CONFIG)
                .write(".intentional/intents/.keep", "")
                .write(
                    "component/package.json",
                    &format!("{{\n  \"name\": \"sample-library\",\n  \"version\": \"{PREVIOUS_VERSION}\"\n}}\n"),
                );
            let root = workspace.root().to_path_buf();
            git(&root, &["init", "--quiet"]);
            git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            git(&root, &["config", "user.name", "Fixture Author"]);
            git(&root, &["config", "user.email", "fixture@example.invalid"]);
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", "Create the workspace"]);
            // The workspace tag carries its own version stream, so the global
            // release tag's baseline is stated rather than projected.
            let baseline = BTreeMap::from([(
                "workspace/release".to_owned(),
                semver::Version::parse(PREVIOUS_VERSION).expect("baseline version"),
            )]);
            crate::tag::TagResult::build_baseline(&root, &baseline)
                .expect("baseline tag set")
                .apply(&root, false)
                .expect("record baseline tags");

            workspace.write(
                ".intentional/intents/quiet-otter-0001.md",
                "---\ncomponent: minor\n---\n\nAdd a component capability\n",
            );
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", "Record release intent"]);
            let source = git(&root, &["rev-parse", "HEAD^{commit}"]);
            let built =
                crate::release::build::build_candidate(&root, &source).expect("release candidate");
            git(
                &root,
                &[
                    "update-ref",
                    &format!("refs/tags/{}", built.tag_name),
                    &built.tag_object,
                ],
            );
            git(
                &root,
                &["checkout", "--quiet", "--detach", &built.release_commit],
            );
            Self { workspace }
        }

        /// Write the readback an npm publication of `version` would leave behind.
        fn observe(&self, version: &str) -> PathBuf {
            let path = self.workspace.root().join("observation.yml");
            std::fs::write(&path, present_document().replace("1.2.3", version))
                .expect("write observation");
            path
        }
    }

    #[test]
    fn an_observation_carrying_the_previous_releases_version_is_rejected() {
        let released = ReleasedWorkspace::new("verify-previous-release-version");
        let observation = released.observe(PREVIOUS_VERSION);
        let clock = clock();
        let output = released.workspace.root().join("evidence.yml");
        let error = verify_publication(&request(
            &released.workspace,
            PublisherKind::Npm,
            None,
            &observation,
            &output,
            &clock,
            &CheckoutContext::new(),
        ))
        .expect_err("the previous release's observation is reported");
        assert!(
            error.to_string().contains(&format!("{PREVIOUS_VERSION:?}"))
                && error.to_string().contains(&format!("{CURRENT_VERSION:?}")),
            "{error}"
        );
        assert!(!output.exists(), "a refused verification writes nothing");
    }

    #[test]
    fn an_observation_carrying_the_reproduced_release_version_is_accepted() {
        let released = ReleasedWorkspace::new("verify-current-release-version");
        let observation = released.observe(CURRENT_VERSION);
        let clock = clock();
        let output = released.workspace.root().join("evidence.yml");
        let verified = verify_publication(&request(
            &released.workspace,
            PublisherKind::Npm,
            None,
            &observation,
            &output,
            &clock,
            &CheckoutContext::new(),
        ))
        .expect("the released version verifies");
        assert_eq!(verified.evidence.subject.version, CURRENT_VERSION);
        assert_eq!(verified.evidence.destination.version, CURRENT_VERSION);
    }

    /// A sealed fragment agreeing with the release the tests resolve.
    fn sealed_fragment(context: &TestContext) -> PublisherEvidence {
        PublisherEvidence {
            schema: PUBLISHER_EVIDENCE_SCHEMA.to_owned(),
            contract: PUBLISHER_EVIDENCE_CONTRACT.to_owned(),
            release_unit: "component".to_owned(),
            package: "package".to_owned(),
            publisher: PublisherKind::Npm,
            target: PRIMARY_TARGET.to_owned(),
            source_commit: context.release.source_commit.clone(),
            release_commit: context.release.release_commit.clone(),
            global_tag: context.release.global_tag.clone(),
            plan_digest: context.release.plan_digest.clone(),
            subject: Subject {
                kind: "npm-package".to_owned(),
                identity: "sample-library".to_owned(),
                version: "1.2.3".to_owned(),
                digest: digest("aa"),
            },
            packager: PackagerRecord {
                id: "npm".to_owned(),
                version: "10.8.0".to_owned(),
            },
            build_provenance: Vec::new(),
            attached_metadata: Vec::new(),
            destination: Destination {
                identity: "npmjs".to_owned(),
                version: "1.2.3".to_owned(),
                digest: digest("aa"),
            },
            clean_client: CleanClient {
                mode: CleanClientMode::Public,
                client: "npm".to_owned(),
                version: "10.8.0".to_owned(),
                digest: digest("aa"),
            },
            destination_aliases: vec![DestinationAlias {
                name: "latest".to_owned(),
                digest: digest("aa"),
            }],
            phase_tags: Vec::new(),
        }
    }

    #[test]
    fn a_sealed_fragment_is_reused_though_aliases_and_tool_versions_advanced() {
        let workspace = npm_workspace("verify-reuse");
        workspace.write(
            "observation.yml",
            &present_document()
                .replace("version: 10.9.0", "version: 11.0.0")
                .replace(
                    "state: present\n",
                    &format!(
                        "state: present
destination-aliases:
  - name: latest
    digest: {}
",
                        digest("99")
                    ),
                ),
        );
        let mut context = TestContext::new();
        let sealed = sealed_fragment(&context);
        context
            .sealed
            .insert("component/package/npm/primary".to_owned(), sealed.clone());
        let clock = clock();
        let output = workspace.root().join("evidence.yml");
        let verified = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &output,
            &clock,
            &context,
        ))
        .expect("the sealed fragment is reused");
        assert!(verified.reused);
        assert_eq!(
            verified.evidence, sealed,
            "the historical claim is unchanged"
        );
        assert_eq!(
            std::fs::read_to_string(&output).expect("fragment"),
            serde_yaml::to_string(&sealed).expect("sealed document"),
            "a reused fragment is written unchanged"
        );
    }

    #[test]
    fn a_sealed_fragment_whose_subject_digest_disagrees_is_rejected() {
        let workspace = npm_workspace("verify-reuse-conflict");
        workspace.write("observation.yml", &present_document());
        let mut context = TestContext::new();
        let mut sealed = sealed_fragment(&context);
        sealed.subject.digest = digest("bb");
        context
            .sealed
            .insert("component/package/npm/primary".to_owned(), sealed);
        let clock = clock();
        let output = workspace.root().join("evidence.yml");
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &output,
            &clock,
            &context,
        ))
        .expect_err("a disagreeing sealed subject is reported");
        assert!(error.to_string().contains("subject digest"), "{error}");
        assert!(!output.exists(), "a refused verification writes nothing");
    }

    #[test]
    fn a_sealed_fragment_bound_to_another_release_is_rejected() {
        let workspace = npm_workspace("verify-reuse-rebound");
        workspace.write("observation.yml", &present_document());
        let mut context = TestContext::new();
        let mut sealed = sealed_fragment(&context);
        sealed.plan_digest = digest("ff");
        context
            .sealed
            .insert("component/package/npm/primary".to_owned(), sealed);
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a rebound sealed fragment is reported");
        assert!(error.to_string().contains("plan digest"), "{error}");
    }

    #[test]
    fn an_observation_of_another_publication_is_rejected() {
        let workspace = npm_workspace("verify-foreign-observation");
        workspace.write(
            "observation.yml",
            &present_document().replace("release-unit: component", "release-unit: other-component"),
        );
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a foreign observation is reported");
        assert!(
            error
                .to_string()
                .contains("describes publication other-component/package/npm/primary"),
            "{error}"
        );
    }

    #[test]
    fn an_observation_at_another_destination_is_rejected() {
        let workspace = npm_workspace("verify-foreign-destination");
        workspace.write(
            "observation.yml",
            &present_document().replace("identity: npmjs", "identity: example-registry"),
        );
        let context = TestContext::new();
        let clock = clock();
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &workspace.root().join("observation.yml"),
            &workspace.root().join("evidence.yml"),
            &clock,
            &context,
        ))
        .expect_err("a foreign destination is reported");
        assert!(error.to_string().contains("example-registry"), "{error}");
    }

    #[test]
    fn a_pending_observation_past_its_deadline_writes_no_fragment() {
        let workspace = npm_workspace("verify-deadline");
        workspace.write(
            "observation.yml",
            &format!(
                "$schema: {PUBLICATION_OBSERVATION_SCHEMA}
contract: {PUBLICATION_OBSERVATION_CONTRACT}
release-unit: component
package: package
publisher: npm
target: primary
state: {}
",
                ObservationState::Pending
            ),
        );
        let context = TestContext::new();
        let clock = clock();
        let observation = workspace.root().join("observation.yml");
        let output = workspace.root().join("evidence.yml");
        let error = verify_publication(&request(
            &workspace,
            PublisherKind::Npm,
            None,
            &observation,
            &output,
            &clock,
            &context,
        ))
        .expect_err("an exhausted deadline is reported");
        assert!(error.to_string().contains("retryable"), "{error}");
        assert!(!output.exists());
    }

    #[test]
    fn a_system_package_uses_its_configured_observation_deadline_when_no_policy_is_supplied() {
        let workspace = workspace(
            "verify-system-package-deadline",
            "    rpm:\n      delivery-action: .github/actions/deliver\n      base-url: https://packages.invalid/rpm\n      public-signing-key-url: https://packages.invalid/key.asc\n      observation-deadline: 47\n      channel: stable\n      with: {}\n",
            &[(
                "component/.goreleaser.yml",
                "version: 2\nproject_name: example-tool\nbuilds:\n  - main: ./cmd/example-tool\nnfpms:\n  - formats: [rpm]\n",
            ),
            ("component/go.mod", "module example.test/example-module\n"),
            ("component/cmd/example-tool/main.go", "package main\n\nfunc main() {}\n")],
        );
        workspace.write(
            "observation.yml",
            &format!(
                "$schema: {PUBLICATION_OBSERVATION_SCHEMA}\ncontract: {PUBLICATION_OBSERVATION_CONTRACT}\nrelease-unit: component\npackage: package\npublisher: rpm\ntarget: primary\nstate: {}\n",
                ObservationState::Pending
            ),
        );
        let context = TestContext::new();
        let clock = clock();
        let observation = workspace.root().join("observation.yml");
        let output = workspace.root().join("evidence.yml");
        let mut request = request(
            &workspace,
            PublisherKind::Rpm,
            None,
            &observation,
            &output,
            &clock,
            &context,
        );
        request.policy = None;
        let error = verify_publication(&request).expect_err("the configured deadline expires");
        assert!(
            error
                .to_string()
                .contains("past its 47 second observation deadline"),
            "{error}"
        );
        assert_eq!(
            clock.elapsed(),
            Duration::ZERO,
            "the recipe already consumed the configured deadline"
        );
        assert!(!output.exists());
    }

    #[test]
    fn phase_tag_evidence_seals_the_same_fragment_shape() {
        let context = TestContext::new();
        let sealed = sealed_fragment(&context);
        let document = serde_yaml::to_string(&PhaseTagEvidence {
            schema: crate::evidence::assemble::PHASE_TAG_EVIDENCE_SCHEMA.to_owned(),
            phase: crate::model::TagPhase::AfterPublication,
            source_commit: context.release.source_commit.clone(),
            release_commit: context.release.release_commit.clone(),
            global_tag: context.release.global_tag.name.clone(),
            plan_digest: context.release.plan_digest.clone(),
            subjects: Vec::new(),
            intended_destinations: None,
            publisher_evidence: Some(vec![sealed.clone()]),
        })
        .expect("phase-tag evidence serializes");
        let decoded: PhaseTagEvidence =
            serde_yaml::from_str(&document).expect("phase-tag evidence round-trips");
        assert_eq!(
            decoded.publisher_evidence.expect("sealed fragments"),
            vec![sealed],
            "a sealed fragment survives the tag record unchanged"
        );
    }
}
