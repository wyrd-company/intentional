// ---
// relationships:
//   implements: github-release-executor
// ---

//! Independent verification of the published global release tag.
//!
//! The initial publication job runs inside a checkout of the released commit
//! with complete history and every tag present, and it holds no credentials.
//! It therefore proves the release from the repository alone: it resolves the
//! one workspace tag that declares no publication phase, requires that tag to
//! be an annotated tag targeting the checked-out commit, reads the Intentional
//! record the tag carries, and rebuilds the whole candidate from the sole
//! parent so the tag, tree, plan digest, projections, changelogs, and consumed
//! intents are proven to agree rather than assumed to.

use crate::config::{Config, UnphasedTag};
use crate::error::{Error, Result};
use crate::plan::ReleasePlan;
use crate::release::build::build_candidate;
use crate::release::git::{self, GitCommand};
use semver::Version;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Intentional record field binding a tag to the sealed release plan.
const PLAN_DIGEST_FIELD: &str = "plan-digest";

/// Intentional record fields every release tag must carry.
const REQUIRED_FIELDS: [&str; 6] = [
    "contract",
    "generator",
    PLAN_DIGEST_FIELD,
    "tag-id",
    "version",
    "baseline",
];

/// A published global release tag that reproduced every release invariant.
#[derive(Debug, Clone)]
pub struct VerifiedReleaseTag {
    /// Accepted source commit S, the sole parent of the release commit.
    pub source: String,
    /// Released commit R the global tag targets.
    pub release: String,
    /// Rendered name of the annotated global release tag.
    pub global_tag: String,
    /// Identity of the annotated global tag object itself.
    ///
    /// Reproduction proves this object, not merely the name pointing at it, so
    /// a consumer that binds evidence to a tag object is binding to something
    /// this verification rebuilt from the accepted source commit.
    pub global_tag_object: String,
    /// Digest sealed inside the release plan the tag binds.
    pub plan_digest: String,
    /// The release plan this verification rebuilt from the accepted source commit.
    ///
    /// The reproduction already seals it against the digest the published tag
    /// binds, so a consumer reading it is reading a plan derived from S and
    /// proved against the tag, not a document some other job transported.
    pub plan: ReleasePlan,
    /// Version the reproduced plan assigns each release unit.
    ///
    /// The reproduction already rebuilds the plan from S, so the version this
    /// release publishes for each unit is proved rather than asserted. It is
    /// what lets a later check reject evidence describing some other release's
    /// subject.
    pub versions: BTreeMap<String, String>,
}

impl VerifiedReleaseTag {
    /// Stable identity lines the credential-free verify-release-tag Action projects.
    pub fn projections(&self) -> Vec<String> {
        vec![
            format!("source-sha: {}", self.source),
            format!("release-sha: {}", self.release),
            format!("global-tag: {}", self.global_tag),
            format!("global-tag-object: {}", self.global_tag_object),
            format!("plan-digest: {}", self.plan_digest),
        ]
    }
}

/// Verify the global release tag the current checkout sits on.
pub fn verify_release_tag(root: &Path) -> Result<VerifiedReleaseTag> {
    let config = Config::load(root)?;
    let configured = global_release_tag(&config)?;
    let release = git::resolve(root, "HEAD^{commit}")?;
    let tag = resolve_published_tag(root, &release, &configured)?;
    let (tree, source) = release_commit_shape(root, &release)?;
    verify_record(&tag, &config, &configured)?;
    let plan_digest = tag
        .fields
        .get(PLAN_DIGEST_FIELD)
        .ok_or_else(|| {
            Error::Validation(format!(
                "the global release tag {} is missing Intentional record field {PLAN_DIGEST_FIELD}",
                tag.name
            ))
        })?
        .clone();
    let plan = reproduce_release(root, &release, &tree, &source, &tag, &plan_digest)?;
    let versions = plan
        .release_units
        .iter()
        .map(|unit| (unit.id.clone(), unit.new_version.clone()))
        .collect();
    Ok(VerifiedReleaseTag {
        source,
        release,
        global_tag: tag.name,
        global_tag_object: tag.object,
        plan_digest,
        plan,
        versions,
    })
}

/// One annotated tag and the Intentional record its message carries.
struct AnnotatedTag {
    /// Rendered tag name the ref publishes.
    name: String,
    /// Identity of the annotated tag object itself.
    object: String,
    /// Commit named in the tag object header.
    target: String,
    /// Record fields decoded from the tag message.
    fields: BTreeMap<String, String>,
}

/// The single workspace tag that declares no publication phase.
///
/// Every other tag, workspace or release-unit, is created later by the
/// publication workflow, so the unphased workspace tag is the one release
/// identity a publication run can already observe, and the constraint is
/// reported here rather than left to fail inside a privileged job.
fn global_release_tag(config: &Config) -> Result<UnphasedTag> {
    let mut workspace = config.unphased_tags();
    match workspace.len() {
        1 => Ok(workspace.remove(0)),
        0 => {
            let release_unit = config.unphased_release_unit_tags();
            if release_unit.is_empty() {
                Err(Error::Validation(
                    "the GitHub executor requires one workspace tag without require-phase as the global release tag; this workspace declares none"
                        .to_owned(),
                ))
            } else {
                Err(Error::Validation(format!(
                    "the GitHub executor requires one workspace tag without require-phase as the global release tag; no workspace tag omits require-phase, and these release-unit tags omit it: {}",
                    release_unit.join(", ")
                )))
            }
        }
        _ => Err(Error::Validation(format!(
            "the GitHub executor requires exactly one workspace tag without require-phase as the global release tag; these workspace tags omit require-phase: {}",
            workspace
                .iter()
                .map(|tag| tag.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Resolve the annotated global release tag the checked-out commit publishes.
fn resolve_published_tag(
    root: &Path,
    release: &str,
    configured: &UnphasedTag,
) -> Result<AnnotatedTag> {
    let (prefix, suffix) = configured.template.split_once("{version}").ok_or_else(|| {
        Error::Validation(format!(
            "configured tag {} has template {:?}, which renders no version",
            configured.id, configured.template
        ))
    })?;
    let listed = GitCommand::new(root)
        .args([
            "for-each-ref",
            "--format=%(refname:strip=2)%00%(objecttype)%00%(objectname)%00%(*objectname)",
            "refs/tags/",
        ])
        .run()?;
    let mut matched = Vec::new();
    for record in listed.text()?.lines().filter(|line| !line.is_empty()) {
        let fields = record.split('\u{0}').collect::<Vec<_>>();
        let [name, kind, object, peeled] = fields.as_slice() else {
            return Err(Error::Git(format!(
                "git for-each-ref produced an unreadable tag record: {record}"
            )));
        };
        let Some(version) = name
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
        else {
            continue;
        };
        if Version::parse(version).is_err() {
            continue;
        }
        // An annotated tag reports its own identity and its peeled commit
        // separately, so the commit a tag publishes is the peeled value where
        // one exists and the ref target otherwise.
        let target = if peeled.is_empty() { object } else { peeled };
        if *target != release {
            continue;
        }
        matched.push(((*name).to_owned(), (*kind).to_owned(), (*object).to_owned()));
    }
    match matched.as_slice() {
        [(name, kind, object)] => {
            if kind != "tag" {
                return Err(Error::Validation(format!(
                    "the global release tag {name} is a lightweight tag rather than an annotated tag object carrying an Intentional release record"
                )));
            }
            read_annotated_tag(root, name, object)
        }
        [] => Err(Error::Validation(format!(
            "the checkout does not sit on a global release tag; no tag rendered from template {:?} targets the checked-out commit {release}",
            configured.template
        ))),
        candidates => Err(Error::Validation(format!(
            "the checked-out commit {release} is targeted by more than one global release tag: {}",
            candidates
                .iter()
                .map(|(name, _, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Decode one annotated tag object into its header target and record fields.
fn read_annotated_tag(root: &Path, name: &str, object: &str) -> Result<AnnotatedTag> {
    let raw = GitCommand::new(root)
        .args(["cat-file", "tag", object])
        .run()?;
    let text = raw.text()?;
    let (header, message) = text.split_once("\n\n").ok_or_else(|| {
        Error::Validation(format!(
            "the global release tag {name} carries no Intentional release record"
        ))
    })?;
    let mut target = None;
    let mut kind = None;
    let mut declared = None;
    for line in header.lines() {
        match line.split_once(' ') {
            Some(("object", value)) => target = Some(value.to_owned()),
            Some(("type", value)) => kind = Some(value.to_owned()),
            Some(("tag", value)) => declared = Some(value.to_owned()),
            _ => {}
        }
    }
    let target = target
        .ok_or_else(|| Error::Git(format!("annotated tag {name} declares no target object")))?;
    if kind.as_deref() != Some("commit") {
        return Err(Error::Validation(format!(
            "the global release tag {name} targets a {} rather than the release commit",
            kind.unwrap_or_else(|| "missing type".to_owned())
        )));
    }
    if declared.as_deref() != Some(name) {
        return Err(Error::Validation(format!(
            "the global release tag ref {name} publishes a tag object that names {}",
            declared.unwrap_or_else(|| "nothing".to_owned())
        )));
    }
    let fields = message
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();
    Ok(AnnotatedTag {
        name: name.to_owned(),
        object: object.to_owned(),
        target,
        fields,
    })
}

/// Prove the record is a complete, non-baseline release record for this workspace.
fn verify_record(tag: &AnnotatedTag, config: &Config, configured: &UnphasedTag) -> Result<()> {
    for field in REQUIRED_FIELDS {
        if !tag.fields.contains_key(field) {
            return Err(Error::Validation(format!(
                "the global release tag {} is missing Intentional record field {field}",
                tag.name
            )));
        }
    }
    for (field, expected) in [
        ("contract", config.contract.as_str()),
        ("tag-id", configured.id.as_str()),
        ("baseline", "false"),
    ] {
        let found = tag.fields[field].as_str();
        if found != expected {
            return Err(Error::Validation(format!(
                "the global release tag {} records {field} {found}; this workspace requires {expected}",
                tag.name
            )));
        }
    }
    Ok(())
}

/// Read the tree and the sole parent of the released commit.
fn release_commit_shape(root: &Path, release: &str) -> Result<(String, String)> {
    let commit = GitCommand::new(root)
        .args(["show", "--no-patch", "--format=%T%n%P", release])
        .run()?;
    let mut lines = commit.text()?.lines();
    let tree = lines.next().unwrap_or_default().trim().to_owned();
    let parents = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match parents.as_slice() {
        [source] => Ok((tree, source.clone())),
        [] => Err(Error::Validation(format!(
            "the release commit {release} must have the accepted source commit as its sole parent; found no parent"
        ))),
        several => Err(Error::Validation(format!(
            "the release commit {release} must have the accepted source commit as its sole parent; found {}",
            several.join(", ")
        ))),
    }
}

/// Rebuild the candidate from the source commit alone and compare every identity.
///
/// Reproduction happens in a throwaway clone so verification never mutates the
/// repository it is proving, and so the release tags the repository already
/// carries can be excluded from the version authority the rebuild derives.
fn reproduce_release(
    root: &Path,
    release: &str,
    tree: &str,
    source: &str,
    tag: &AnnotatedTag,
    plan_digest: &str,
) -> Result<ReleasePlan> {
    if tag.target != release {
        return Err(Error::Validation(format!(
            "the global release tag {} targets {}, not the checked-out release commit {release}",
            tag.name, tag.target
        )));
    }
    let reproduction_root = tempfile::Builder::new()
        .prefix("intentional-release-tag-reproduce")
        .tempdir()
        .map_err(|error| Error::Git(format!("failed to create a reproduction clone: {error}")))?;
    let clone = isolated_clone(root, reproduction_root.path(), source)?;
    GitCommand::new(&clone)
        .args(["checkout", "--quiet", "--force", "--detach", source])
        .run()?;
    discard_released_tags(&clone, release)?;

    let built = build_candidate(&clone, source)?;
    if built.plan.digest != plan_digest {
        return Err(Error::Validation(format!(
            "the global release tag {} binds plan digest {plan_digest} but rebuilding the release from source commit {source} seals {}; the verifying checkout must carry the release tags that hold version authority",
            tag.name, built.plan.digest
        )));
    }
    if built.release_tree != tree {
        return Err(Error::Validation(format!(
            "the reproduced release tree {} does not match the released tree {tree}; the released projections, changelogs, or consumed intents disagree with the sealed plan",
            built.release_tree
        )));
    }
    if built.release_commit != release {
        return Err(Error::Validation(format!(
            "the reproduced release commit {} does not match the released commit {release}",
            built.release_commit
        )));
    }
    if built.tag_object != tag.object || built.tag_name != tag.name {
        return Err(Error::Validation(format!(
            "the reproduced annotated global release tag does not match the published tag {}",
            tag.name
        )));
    }
    Ok(built.plan)
}

/// Create an isolated clone that already contains the accepted source commit.
///
/// Tags carry the version authority the release plan is derived from, so an
/// isolated clone must keep them to reproduce a candidate faithfully.
fn isolated_clone(root: &Path, into: &Path, source: &str) -> Result<PathBuf> {
    let target = into.join("repository");
    let target_argument = target
        .to_str()
        .ok_or_else(|| Error::Git("the clone path is not valid UTF-8".to_owned()))?
        .to_owned();
    let origin = root
        .canonicalize()
        .map_err(|error| Error::io(root, error))?;
    let origin_argument = origin
        .to_str()
        .ok_or_else(|| Error::Git("the repository path is not valid UTF-8".to_owned()))?
        .to_owned();
    GitCommand::new(into)
        .args([
            "clone",
            "--quiet",
            "--no-checkout",
            &origin_argument,
            &target_argument,
        ])
        .run()?;
    if !git::has_object(&target, source)? {
        GitCommand::new(&target)
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                &origin_argument,
                "+refs/*:refs/intentional/source/*",
            ])
            .run()?;
    }
    if !git::has_object(&target, source)? {
        return Err(Error::Validation(format!(
            "the verifying repository does not contain the accepted source commit {source}"
        )));
    }
    Ok(target)
}

/// Remove every tag the release itself created from the reproduction clone.
///
/// The release commit did not exist when the release plan was sealed, so a tag
/// resolving to it is an output of this release rather than an input to it.
/// Leaving those tags in place would let the release under verification supply
/// the version authority it is supposed to be derived from, and a resumed
/// publication would then reproduce a different plan than the one it is
/// verifying. Only tags that already resolve to the release commit are removed,
/// so no tag record can steer the reproduction environment.
fn discard_released_tags(clone: &Path, release: &str) -> Result<()> {
    let listed = GitCommand::new(clone)
        .args([
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(*objectname)",
            "refs/tags/",
        ])
        .run()?;
    let mut released = Vec::new();
    for record in listed.text()?.lines().filter(|line| !line.is_empty()) {
        let fields = record.split('\u{0}').collect::<Vec<_>>();
        let [reference, object, peeled] = fields.as_slice() else {
            return Err(Error::Git(format!(
                "git for-each-ref produced an unreadable tag record: {record}"
            )));
        };
        let target = if peeled.is_empty() { object } else { peeled };
        if *target == release {
            released.push(((*reference).to_owned(), (*object).to_owned()));
        }
    }
    for (reference, object) in released {
        GitCommand::new(clone)
            .args(["update-ref", "-d", &reference, &object])
            .run()?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    const CONFIG: &str = "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nworkspace-tags:\n  release:\n    template: '{version}'\nrelease-units:\n  widget:\n    path: .\n    projections:\n      - adapter: json\n        file: package.json\n        pointer: /version\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: 'widget@{version}'\n        require-phase: after-publication\n";

    const RECORD_TAG_ID: &str = "workspace/release";

    /// A git workspace carrying one applied release and its published global tag.
    ///
    /// Reused by every module whose behaviour is defined against a genuinely
    /// released checkout, because building one from parts is what lets a test
    /// prove a release identity the repository never actually carried.
    pub(crate) struct ReleasedWorkspace {
        #[allow(dead_code)]
        temp: tempfile::TempDir,
        pub(crate) root: PathBuf,
        /// Accepted source commit S.
        pub(crate) source: String,
        /// Deterministic release commit R.
        pub(crate) release: String,
        /// Identity of the annotated global tag object.
        pub(crate) tag_object: String,
        /// Rendered name of the global release tag.
        pub(crate) tag_name: String,
        /// Digest sealed inside the release plan.
        pub(crate) plan_digest: String,
    }

    fn git(directory: &Path, arguments: &[&str]) -> String {
        GitCommand::new(directory)
            .args(arguments)
            .run()
            .unwrap_or_else(|error| panic!("git {arguments:?} failed: {error}"))
            .line()
            .expect("git output")
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent directory"))
            .expect("create parent directory");
        std::fs::write(path, contents).expect("write fixture file");
    }

    impl ReleasedWorkspace {
        /// Author one intent, build the release, and publish its annotated global tag.
        pub(crate) fn new() -> Self {
            Self::with(
                CONFIG,
                &[("package.json", "{\n  \"version\": \"1.0.0\"\n}\n")],
                "widget",
            )
        }

        /// The same released workspace under a caller's configuration.
        ///
        /// A module whose behaviour depends on what the release publishes needs
        /// its own release units and publishers, and building the release from
        /// parts is exactly what lets a test prove an identity the repository
        /// never carried. Parameterising the fixture keeps those tests on a
        /// genuinely released checkout.
        pub(crate) fn with(config: &str, files: &[(&str, &str)], release_unit: &str) -> Self {
            let temp = tempfile::tempdir().expect("temporary directory");
            let root = temp.path().join("workspace");
            std::fs::create_dir_all(&root).expect("create workspace");
            git(&root, &["init", "--quiet", "--initial-branch=main"]);
            git(&root, &["config", "user.name", "Fixture Author"]);
            git(&root, &["config", "user.email", "fixture@example.invalid"]);
            write(&root, ".intentional/config.yml", config);
            for (path, contents) in files {
                write(&root, path, contents);
            }
            write(&root, ".intentional/intents/.keep", "");
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", "Create the workspace"]);
            // A workspace tag carries its own version stream, so the baseline
            // states where that stream starts rather than deriving it from a
            // release unit.
            let baseline = Config::load(&root)
                .expect("the fixture configuration loads")
                .unphased_tags()
                .into_iter()
                .map(|tag| (tag.id, "1.0.0".parse().expect("version")))
                .collect::<BTreeMap<_, _>>();
            crate::tag::TagResult::build_baseline(&root, &baseline)
                .expect("baseline tag set")
                .apply(&root, false)
                .expect("record baseline tags");

            write(
                &root,
                ".intentional/intents/quiet-otter-0001.md",
                &format!("---\n{release_unit}: minor\n---\n\nAdd a capability\n"),
            );
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", "Record release intent"]);

            let source = git(&root, &["rev-parse", "HEAD^{commit}"]);
            let built = build_candidate(&root, &source).expect("release candidate");
            let workspace = Self {
                temp,
                root,
                source,
                release: built.release_commit.clone(),
                tag_object: built.tag_object.clone(),
                tag_name: built.tag_name.clone(),
                plan_digest: built.plan.digest.clone(),
            };
            workspace.publish(&workspace.tag_name.clone(), &built.tag_object);
            workspace.checkout(&built.release_commit);
            workspace
        }

        /// Record one tag ref under `name`.
        fn publish(&self, name: &str, object: &str) {
            git(
                &self.root,
                &["update-ref", &format!("refs/tags/{name}"), object],
            );
        }

        /// Remove the published global tag so a forgery can replace it.
        fn unpublish(&self) {
            git(
                &self.root,
                &["update-ref", "-d", &format!("refs/tags/{}", self.tag_name)],
            );
        }

        pub(crate) fn checkout(&self, commit: &str) {
            git(&self.root, &["checkout", "--quiet", "--detach", commit]);
        }

        /// Build an annotated tag object from a complete raw tag body.
        fn mktag(&self, body: &str) -> String {
            GitCommand::new(&self.root)
                .arg("mktag")
                .stdin(body.to_owned().into_bytes())
                .run()
                .expect("annotated tag object")
                .line()
                .expect("tag identity")
        }

        /// The tagger line of the published global tag, reused by forged tags.
        fn tagger(&self) -> String {
            GitCommand::new(&self.root)
                .args(["cat-file", "tag", &self.tag_object])
                .run()
                .expect("published tag object")
                .text()
                .expect("tag text")
                .lines()
                .find(|line| line.starts_with("tagger "))
                .expect("tagger line")
                .to_owned()
        }

        /// The published tagger line rewritten to a different identity.
        ///
        /// The moment is held fixed so the tagger is the only difference a
        /// forged tag object carries.
        fn other_tagger(&self) -> String {
            let published = self.tagger();
            let mut trailing = published.rsplitn(3, ' ');
            let zone = trailing.next().expect("tagger time zone");
            let seconds = trailing.next().expect("tagger timestamp");
            format!("tagger Other Fixture <other@example.invalid> {seconds} {zone}")
        }

        /// A well-formed release record body over `target` named `name`.
        fn record(&self, target: &str, name: &str, digest: &str) -> String {
            self.record_as(target, name, digest, &self.tagger())
        }

        /// A well-formed release record body carrying an arbitrary tagger line.
        fn record_as(&self, target: &str, name: &str, digest: &str, tagger: &str) -> String {
            format!(
                "object {target}\ntype commit\ntag {name}\n{tagger}\n\nintentional release record\n\ncontract: contract-1\ngenerator: intentional {}\nplan-digest: {digest}\ntag-id: {RECORD_TAG_ID}\nversion: 1.1.0\nbaseline: false\n",
                crate::VERSION
            )
        }

        fn verify(&self) -> Result<VerifiedReleaseTag> {
            verify_release_tag(&self.root)
        }
    }

    #[test]
    fn verifies_a_published_global_release_tag() {
        let workspace = ReleasedWorkspace::new();
        let verified = workspace.verify().expect("verified release tag");
        assert_eq!(verified.source, workspace.source);
        assert_eq!(verified.release, workspace.release);
        assert_eq!(verified.global_tag, workspace.tag_name);
        assert_eq!(verified.plan_digest, workspace.plan_digest);
        assert_eq!(verified.global_tag_object, workspace.tag_object);
        assert_eq!(
            verified.projections(),
            vec![
                format!("source-sha: {}", workspace.source),
                format!("release-sha: {}", workspace.release),
                format!("global-tag: {}", workspace.tag_name),
                format!("global-tag-object: {}", workspace.tag_object),
                format!("plan-digest: {}", workspace.plan_digest),
            ]
        );
    }

    /// The projected keys, the Action's outputs, and the projector's key list.
    ///
    /// Three documents have to spell one set and nothing joins them. A key this
    /// command projects that the Action does not request is silently dropped;
    /// one the Action declares an output for but never requests resolves empty
    /// in a consumer's `needs` expression. Neither is a parse error and neither
    /// fails a run that never reads the value, so the agreement is asserted
    /// here rather than discovered by a consumer.
    #[test]
    fn projects_exactly_the_identities_its_action_declares_and_requests() {
        let projected = ReleasedWorkspace::new()
            .verify()
            .expect("verified release tag")
            .projections()
            .iter()
            .map(|line| {
                line.split_once(": ")
                    .expect("a projection is a key: value line")
                    .0
                    .to_owned()
            })
            .collect::<BTreeSet<_>>();

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../actions/verify-release-tag/action.yml");
        let document: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(&path).expect("the Action document is readable"),
        )
        .expect("the Action document parses");

        let declared = document
            .get("outputs")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("the Action declares outputs")
            .keys()
            .filter_map(serde_yaml::Value::as_str)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            declared, projected,
            "the Action declares an output for exactly the identities the command projects"
        );

        // The requested keys are the ones the projector validates and writes.
        // Reading them out of the rendered step body rather than repeating them
        // keeps this bound to what the Action actually runs.
        let body = document["runs"]["steps"]
            .as_sequence()
            .expect("the Action runs steps")
            .iter()
            .filter_map(|step| step.get("run").and_then(serde_yaml::Value::as_str))
            .find(|run| run.contains("project-identities.sh"))
            .expect("the Action projects through the shared projector");
        let requested = body
            .rsplit_once("project-identities.sh")
            .expect("the projector invocation")
            .1
            .split_whitespace()
            // Everything the invocation carries besides its keys is shell: the
            // closing quote of the projector path, the quoted output file, and
            // the line continuations. A key is what is left, so a mistyped one
            // survives this filter and fails the comparison rather than being
            // quietly skipped by a pattern that only matches valid keys.
            .filter(|word| !word.contains(['"', '$', '\\']))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            requested, projected,
            "the Action requests exactly the identities the command projects"
        );
    }

    /// Reproduction supplies every identity a prepared handoff would carry.
    ///
    /// This is the question that decides whether evidence assembly needs a
    /// handoff input at all. A `release-candidate.yml` states S, R, the global
    /// tag's name, object and target, the sealed plan digest, and transports the
    /// plan itself. Verification derives all seven from the checkout it proves,
    /// so the handoff would be restating what reproduction already establishes.
    ///
    /// The tag's target is not a seventh value: reproduction refuses a tag that
    /// targets anything but the released commit, so the target *is* R.
    #[test]
    fn reproduces_every_identity_a_prepared_handoff_would_state() {
        let workspace = ReleasedWorkspace::new();
        let verified = workspace.verify().expect("verified release tag");

        assert_eq!(verified.source, workspace.source, "S");
        assert_eq!(verified.release, workspace.release, "R");
        assert_eq!(verified.global_tag, workspace.tag_name, "the tag name");
        assert_eq!(
            verified.global_tag_object, workspace.tag_object,
            "the tag object"
        );
        assert_eq!(
            verified.plan_digest, workspace.plan_digest,
            "the plan digest"
        );

        // The plan is the one the tag seals, and it is sealed over its own
        // payload, so a consumer can bind evidence to it without transporting
        // a document alongside.
        assert_eq!(verified.plan.digest, workspace.plan_digest);
        verified
            .plan
            .verify_digest()
            .expect("the reproduced plan seals its own payload");
        assert_eq!(
            verified.plan.payload_digest().expect("recompute"),
            workspace.plan_digest,
            "the digest is recomputable from the reproduced payload"
        );

        // The global tag the configuration names is a tag this plan seals, which
        // is the lookup evidence assembly performs against the handoff today.
        let configured = global_release_tag(&Config::load(&workspace.root).expect("config"))
            .expect("one global release tag");
        let sealed = verified
            .plan
            .tags
            .iter()
            .find(|tag| tag.id == configured.id)
            .expect("the reproduced plan seals the configured global release tag");
        assert_eq!(sealed.name, verified.global_tag);
    }

    #[test]
    fn leaves_the_verified_repository_unchanged() {
        let workspace = ReleasedWorkspace::new();
        let before = git(&workspace.root, &["for-each-ref", "--format=%(refname)"]);
        workspace.verify().expect("verified release tag");
        let after = git(&workspace.root, &["for-each-ref", "--format=%(refname)"]);
        assert_eq!(before, after);
    }

    #[test]
    fn rejects_a_checkout_that_the_release_tag_does_not_target() {
        let workspace = ReleasedWorkspace::new();
        workspace.checkout(&workspace.source);
        let error = workspace
            .verify()
            .expect_err("the checkout is not the released commit");
        assert!(
            error
                .to_string()
                .contains("does not sit on a global release tag"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_release_commit_with_more_than_one_parent() {
        let workspace = ReleasedWorkspace::new();
        let tree = git(
            &workspace.root,
            &["rev-parse", &format!("{}^{{tree}}", workspace.release)],
        );
        let earlier = git(
            &workspace.root,
            &["rev-parse", &format!("{}~1", workspace.source)],
        );
        let merged = GitCommand::new(&workspace.root)
            .args([
                "commit-tree",
                &tree,
                "-p",
                &workspace.source,
                "-p",
                &earlier,
            ])
            .stdin(b"chore(release): apply 1.1.0".to_vec())
            .run()
            .expect("merge commit")
            .line()
            .expect("merge identity");
        let object = workspace.mktag(&workspace.record(&merged, "1.2.0", &workspace.plan_digest));
        workspace.publish("1.2.0", &object);
        workspace.checkout(&merged);
        let error = workspace
            .verify()
            .expect_err("a release commit has one parent");
        assert!(error.to_string().contains("sole parent"), "{error}");
    }

    #[test]
    fn rejects_a_record_whose_plan_digest_disagrees() {
        let workspace = ReleasedWorkspace::new();
        let object = workspace.mktag(&workspace.record(
            &workspace.release,
            &workspace.tag_name,
            &format!("sha256:{}", "0".repeat(64)),
        ));
        workspace.unpublish();
        workspace.publish(&workspace.tag_name.clone(), &object);
        let error = workspace
            .verify()
            .expect_err("the recorded digest is not the sealed digest");
        assert!(error.to_string().contains("binds plan digest"), "{error}");
    }

    #[test]
    fn rejects_a_projection_that_no_longer_matches_the_sealed_plan() {
        let workspace = ReleasedWorkspace::new();
        let blob = GitCommand::new(&workspace.root)
            .args(["hash-object", "-w", "--stdin", "--path", "package.json"])
            .stdin(b"{\n  \"version\": \"9.9.9\"\n}\n".to_vec())
            .run()
            .expect("forged projection blob")
            .line()
            .expect("blob identity");
        let index = workspace.temp.path().join("forged-index");
        let index = index.to_str().expect("index path").to_owned();
        GitCommand::new(&workspace.root)
            .args(["read-tree", &workspace.release])
            .env("GIT_INDEX_FILE", &index)
            .run()
            .expect("read the released tree");
        GitCommand::new(&workspace.root)
            .args(["update-index", "--index-info"])
            .env("GIT_INDEX_FILE", &index)
            .stdin(format!("100644 {blob}\tpackage.json\n").into_bytes())
            .run()
            .expect("forge the projection");
        let tree = GitCommand::new(&workspace.root)
            .arg("write-tree")
            .env("GIT_INDEX_FILE", &index)
            .run()
            .expect("forged tree")
            .line()
            .expect("tree identity");
        let forged = GitCommand::new(&workspace.root)
            .args(["commit-tree", &tree, "-p", &workspace.source])
            .stdin(b"chore(release): apply 1.1.0".to_vec())
            .run()
            .expect("forged release commit")
            .line()
            .expect("commit identity");
        let object = workspace.mktag(&workspace.record(
            &forged,
            &workspace.tag_name,
            &workspace.plan_digest,
        ));
        workspace.unpublish();
        workspace.publish(&workspace.tag_name.clone(), &object);
        workspace.checkout(&forged);
        let error = workspace
            .verify()
            .expect_err("the released projection was rewritten");
        assert!(
            error
                .to_string()
                .contains("does not match the released tree"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_release_commit_rewritten_around_the_reproduced_tree() {
        let workspace = ReleasedWorkspace::new();
        let tree = git(
            &workspace.root,
            &["rev-parse", &format!("{}^{{tree}}", workspace.release)],
        );
        let forged = GitCommand::new(&workspace.root)
            .args(["commit-tree", &tree, "-p", &workspace.source])
            .stdin(b"chore(release): ship 1.1.0".to_vec())
            .run()
            .expect("forged release commit")
            .line()
            .expect("commit identity");
        let object = workspace.mktag(&workspace.record(
            &forged,
            &workspace.tag_name,
            &workspace.plan_digest,
        ));
        workspace.unpublish();
        workspace.publish(&workspace.tag_name.clone(), &object);
        workspace.checkout(&forged);
        let error = workspace
            .verify()
            .expect_err("the release commit was re-worded");
        assert!(
            error
                .to_string()
                .contains("does not match the released commit"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_release_tag_re_created_by_another_tagger() {
        let workspace = ReleasedWorkspace::new();
        let object = workspace.mktag(&workspace.record_as(
            &workspace.release,
            &workspace.tag_name,
            &workspace.plan_digest,
            &workspace.other_tagger(),
        ));
        assert_ne!(
            object, workspace.tag_object,
            "a re-tagged record must be a different tag object"
        );
        workspace.unpublish();
        workspace.publish(&workspace.tag_name.clone(), &object);
        let error = workspace.verify().expect_err("the record was re-tagged");
        assert!(
            error
                .to_string()
                .contains("does not match the published tag"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_lightweight_tag_under_the_global_tag_name() {
        let workspace = ReleasedWorkspace::new();
        workspace.unpublish();
        workspace.publish(&workspace.tag_name.clone(), &workspace.release);
        let error = workspace
            .verify()
            .expect_err("a lightweight tag carries no release record");
        assert!(error.to_string().contains("lightweight tag"), "{error}");
    }

    #[test]
    fn rejects_configuration_with_no_unphased_tags_anywhere() {
        const CONFIG: &str = "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nworkspace-tags:\n  release:\n    template: '{version}'\n    require-phase: after-publication\nrelease-units:\n  widget:\n    path: .\n    projections:\n      - adapter: json\n        file: package.json\n        pointer: /version\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: 'widget@{version}'\n        require-phase: after-publication\n";
        let message = verify_config_error(CONFIG);
        assert!(message.contains("declares none"), "{message}");
        assert!(!message.contains("release-unit"), "{message}");
    }

    #[test]
    fn rejects_configuration_with_unphased_release_unit_tags_but_no_workspace_tag() {
        const CONFIG: &str = "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nrelease-units:\n  intentional:\n    path: .\n    projections:\n      - adapter: json\n        file: package.json\n        pointer: /version\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: '{version}'\n";
        let message = verify_config_error(CONFIG);
        assert!(
            message.contains("no workspace tag omits require-phase"),
            "{message}"
        );
        assert!(
            message.contains("release-unit/intentional/primary"),
            "{message}"
        );
        assert!(!message.contains("declares none"), "{message}");
    }

    #[test]
    fn requires_exactly_one_workspace_tag_without_a_phase() {
        let workspace = ReleasedWorkspace::new();
        write(
            &workspace.root,
            ".intentional/config.yml",
            &CONFIG.replace(
                "  release:\n    template: '{version}'\n",
                "  release:\n    template: '{version}'\n    require-phase: after-publication\n",
            ),
        );
        let error = workspace
            .verify()
            .expect_err("no unphased workspace tag is configured");
        assert!(error.to_string().contains("declares none"), "{error}");

        write(
            &workspace.root,
            ".intentional/config.yml",
            &CONFIG.replace(
                "  release:\n    template: '{version}'\n",
                "  mirror:\n    template: '{version}-mirror'\n  release:\n    template: '{version}'\n",
            ),
        );
        let error = workspace
            .verify()
            .expect_err("two unphased workspace tags are configured");
        assert!(
            error
                .to_string()
                .contains("workspace tags omit require-phase: workspace/mirror, workspace/release"),
            "{error}"
        );
    }

    fn verify_config_error(config_yaml: &str) -> String {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join(".intentional")).expect("create config directory");
        std::fs::write(root.join(".intentional/config.yml"), config_yaml).expect("write config");
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        verify_release_tag(&root)
            .expect_err("configuration is invalid")
            .to_string()
    }
}
