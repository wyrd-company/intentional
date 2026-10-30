// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Tag-derived current versions and intent-derived next versions.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{Bump, ReleaseUnitDisposition};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// Current and next version of a release unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseUnitVersion {
    /// Latest final version tag on the first-parent history.
    pub current: Version,
    /// Current version after the aggregate intent bump.
    pub next: Version,
    /// Aggregate intent bump.
    pub bump: Bump,
    /// Whether this release graduates the release unit to `1.0.0`.
    pub graduates: bool,
}

impl ReleaseUnitVersion {
    /// Compute release-unit versions from a current tag and aggregate bump.
    pub fn new(current: Version, bump: Bump) -> Self {
        let next = bump_version(&current, bump);
        Self {
            current,
            next,
            bump,
            graduates: false,
        }
    }
}

/// Aggregate intent bumps by taking the maximum significance for each release unit.
pub fn aggregate_bumps<'a>(
    intents: impl IntoIterator<Item = &'a BTreeMap<String, Bump>>,
) -> BTreeMap<String, Bump> {
    let mut aggregate: BTreeMap<String, Bump> = BTreeMap::new();
    for intent in intents {
        for (release_unit, bump) in intent {
            aggregate
                .entry(release_unit.clone())
                .and_modify(|existing| *existing = (*existing).max(*bump))
                .or_insert(*bump);
        }
    }
    aggregate
}

/// Collect every release unit that at least one intent graduates.
pub fn aggregate_graduations<'a>(
    intents: impl IntoIterator<Item = &'a BTreeSet<String>>,
) -> BTreeSet<String> {
    intents.into_iter().flatten().cloned().collect()
}

/// Propagate the configured minimum bump to internal dependents in a shared ecosystem.
pub fn effective_bumps(
    config: &Config,
    declared: &BTreeMap<String, Bump>,
) -> BTreeMap<String, Bump> {
    let mut effective = declared
        .iter()
        .filter(|(id, _)| config.release_units.contains_key(*id))
        .map(|(id, bump)| (id.clone(), *bump))
        .collect::<BTreeMap<_, _>>();
    for id in config.release_units.keys() {
        effective.entry(id.clone()).or_default();
    }

    loop {
        let mut changed = false;
        for (id, release_unit) in &config.release_units {
            for dependency in &release_unit.depends_on {
                if effective[dependency] == Bump::None || !share_ecosystem(config, id, dependency) {
                    continue;
                }
                let required = config.settings.internal_dependency_bump;
                if effective[id] < required {
                    effective.insert(id.clone(), required);
                    changed = true;
                }
            }
        }
        for group in &config.fixed {
            let group_bump = group
                .iter()
                .map(|id| effective[id])
                .max()
                .unwrap_or(Bump::None);
            if group_bump != Bump::None {
                for id in group {
                    if effective[id] < group_bump {
                        effective.insert(id.clone(), group_bump);
                        changed = true;
                    }
                }
            }
        }
        for group in &config.linked {
            let group_bump = group
                .iter()
                .map(|id| effective[id])
                .max()
                .unwrap_or(Bump::None);
            if group_bump != Bump::None {
                for id in group {
                    if effective[id] != Bump::None && effective[id] < group_bump {
                        effective.insert(id.clone(), group_bump);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    effective
}

/// Resolve current and next versions after dependency and release-group constraints.
///
/// A graduating release unit releases exactly `1.0.0`. In a fixed group, one
/// graduating member graduates every member. In a linked group, it graduates
/// the releasing members. Graduation is valid only while the version it
/// applies to is `0.x`.
pub fn resolve_versions(
    config: &Config,
    declared: &BTreeMap<String, Bump>,
    graduating: &BTreeSet<String>,
    current: &BTreeMap<String, Version>,
) -> Result<BTreeMap<String, ReleaseUnitVersion>> {
    resolve_versions_with(config, declared, graduating, current, bump_version)
}

/// Resolve versions with another reading of bump names. Initialization uses
/// [`component_bump`] to compute what a component-style tool would release.
pub(crate) fn resolve_versions_with(
    config: &Config,
    declared: &BTreeMap<String, Bump>,
    graduating: &BTreeSet<String>,
    current: &BTreeMap<String, Version>,
    bump: fn(&Version, Bump) -> Version,
) -> Result<BTreeMap<String, ReleaseUnitVersion>> {
    let effective = effective_bumps(config, declared);
    for (id, bump) in &effective {
        if *bump != Bump::None
            && config.release_units[id].disposition == ReleaseUnitDisposition::Suspended
        {
            return Err(Error::Validation(format!(
                "release requires suspended release unit {id}"
            )));
        }
    }
    for id in graduating {
        if config.release_units.contains_key(id) && effective[id] != Bump::Major {
            return Err(Error::Validation(format!(
                "release unit {id} graduates, which requires bump major"
            )));
        }
    }

    let mut resolved = BTreeMap::new();
    let mut grouped = BTreeMap::<String, (&str, usize)>::new();
    for (index, group) in config.fixed.iter().enumerate() {
        for id in group {
            grouped.insert(id.clone(), ("fixed", index));
        }
    }
    for (index, group) in config.linked.iter().enumerate() {
        for id in group {
            grouped.insert(id.clone(), ("linked", index));
        }
    }

    for (id, release_unit) in &config.release_units {
        if grouped.contains_key(id) {
            continue;
        }
        let current_version = current.get(id).ok_or_else(|| {
            Error::Validation(format!("missing current version for release unit {id}"))
        })?;
        let versions = if graduating.contains(id) {
            graduation(id, current_version)?
        } else {
            ReleaseUnitVersion {
                current: current_version.clone(),
                next: bump(current_version, effective[id]),
                bump: effective[id],
                graduates: false,
            }
        };
        resolved.insert(id.clone(), versions);
        if release_unit.disposition == ReleaseUnitDisposition::Suspended {
            debug_assert_eq!(effective[id], Bump::None);
        }
    }

    for group in &config.fixed {
        resolve_group(
            group,
            true,
            &effective,
            graduating,
            current,
            bump,
            &mut resolved,
        )?;
    }
    for group in &config.linked {
        resolve_group(
            group,
            false,
            &effective,
            graduating,
            current,
            bump,
            &mut resolved,
        )?;
    }
    Ok(resolved)
}

fn graduation(id: &str, current: &Version) -> Result<ReleaseUnitVersion> {
    ensure_graduation_source(id, current)?;
    Ok(ReleaseUnitVersion {
        current: current.clone(),
        next: Version::new(1, 0, 0),
        bump: Bump::Major,
        graduates: true,
    })
}

fn ensure_graduation_source(id: &str, version: &Version) -> Result<()> {
    if version.major != 0 {
        return Err(Error::Validation(format!(
            "release unit {id} graduates, but its version is {version}; graduate is valid only at 0.x"
        )));
    }
    Ok(())
}

fn resolve_group(
    group: &[String],
    fixed: bool,
    effective: &BTreeMap<String, Bump>,
    graduating: &BTreeSet<String>,
    current: &BTreeMap<String, Version>,
    bump: fn(&Version, Bump) -> Version,
    resolved: &mut BTreeMap<String, ReleaseUnitVersion>,
) -> Result<()> {
    let highest_current = group
        .iter()
        .map(|id| {
            current.get(id).ok_or_else(|| {
                Error::Validation(format!("missing current version for release unit {id}"))
            })
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .expect("validated groups are non-empty")
        .clone();
    let highest_bump = group
        .iter()
        .map(|id| effective[id])
        .max()
        .unwrap_or(Bump::None);
    let graduate = group.iter().find(|id| graduating.contains(*id));
    let shared_next = match graduate {
        Some(id) => {
            // The group shares one version line, so its highest current
            // version is the one that graduates.
            ensure_graduation_source(id, &highest_current)?;
            Version::new(1, 0, 0)
        }
        None => bump(&highest_current, highest_bump),
    };
    for id in group {
        let member_current = current[id].clone();
        let releases = highest_bump != Bump::None && (fixed || effective[id] != Bump::None);
        resolved.insert(
            id.clone(),
            ReleaseUnitVersion {
                current: member_current.clone(),
                next: if releases {
                    shared_next.clone()
                } else {
                    member_current
                },
                bump: if releases { highest_bump } else { Bump::None },
                graduates: releases && graduate.is_some(),
            },
        );
    }
    Ok(())
}

fn share_ecosystem(config: &Config, left: &str, right: &str) -> bool {
    config.release_units[left]
        .projections
        .iter()
        .any(|left_projection| {
            left_projection.adapter.ecosystem().is_some()
                && config.release_units[right]
                    .projections
                    .iter()
                    .any(|right_projection| {
                        left_projection.adapter.ecosystem() == right_projection.adapter.ecosystem()
                    })
        })
}

/// Apply a bump that states change significance.
///
/// For a `0.x.y` version, `major` advances the compatibility boundary to
/// `0.(x+1).0`, while both `minor` and `patch` advance to `0.x.(y+1)`. This
/// matches caret-range compatibility semantics across npm, Cargo, and Pub.
/// At `1.0.0` and later, ordinary SemVer applies. No bump reaches `1.0.0`
/// from `0.x`: only an intent that sets `graduate` does, through
/// [`resolve_versions`].
pub fn bump_version(current: &Version, bump: Bump) -> Version {
    let mut next = current.clone();
    next.pre = semver::Prerelease::EMPTY;
    next.build = semver::BuildMetadata::EMPTY;
    if next.major == 0 {
        match bump {
            Bump::None => {}
            Bump::Major => {
                next.minor += 1;
                next.patch = 0;
            }
            Bump::Minor | Bump::Patch => next.patch += 1,
        }
        return next;
    }
    match bump {
        Bump::None => {}
        Bump::Patch => next.patch += 1,
        Bump::Minor => {
            next.minor += 1;
            next.patch = 0;
        }
        Bump::Major => {
            next.major += 1;
            next.minor = 0;
            next.patch = 0;
        }
    }
    next
}

/// Increment the named version component, as Changesets and `contract-1`
/// component configs read bump names.
pub(crate) fn component_bump(current: &Version, bump: Bump) -> Version {
    let mut next = current.clone();
    next.pre = semver::Prerelease::EMPTY;
    next.build = semver::BuildMetadata::EMPTY;
    match bump {
        Bump::None => {}
        Bump::Patch => next.patch += 1,
        Bump::Minor => {
            next.minor += 1;
            next.patch = 0;
        }
        Bump::Major => {
            next.major += 1;
            next.minor = 0;
            next.patch = 0;
        }
    }
    next
}

/// Read release-unit versions and heights from Git using tagver traversal semantics.
pub struct VersionRepository {
    repository: tagver::Repository,
}

impl VersionRepository {
    /// Discover a Git repository from `path`.
    pub fn discover(path: &Path) -> Result<Self> {
        let repository =
            tagver::Repository::discover(path).map_err(|error| Error::Git(error.to_string()))?;
        Ok(Self { repository })
    }

    /// Find the latest final SemVer matching a release-unit tag template.
    pub fn current_version(&self, release_unit_id: &str, template: &str) -> Result<Version> {
        let matches = self.matching_tags(release_unit_id, template)?;
        let (version, _) = self.walk_to_tag(&matches, false)?;
        Ok(version.unwrap_or_else(|| Version::new(0, 0, 0)))
    }

    /// Find the latest final SemVer before tags created at `excluded_target`.
    pub fn current_version_before(
        &self,
        release_unit_id: &str,
        template: &str,
        excluded_target: gix::ObjectId,
    ) -> Result<Version> {
        let mut matches = self.matching_tags(release_unit_id, template)?;
        matches.remove(&excluded_target);
        let (version, _) = self.walk_to_tag(&matches, false)?;
        Ok(version.unwrap_or_else(|| Version::new(0, 0, 0)))
    }

    /// Return whether at least one matching tag exists on the repository history.
    pub fn has_matching_tag(&self, release_unit_id: &str, template: &str) -> Result<bool> {
        Ok(!self.matching_tags(release_unit_id, template)?.is_empty())
    }

    /// Count commits on the first-parent chain since the latest matching tag.
    pub fn height(&self, release_unit_id: &str, template: &str) -> Result<u64> {
        let matches = self.matching_tags(release_unit_id, template)?;
        let (_, height) = self.walk_to_tag(&matches, true)?;
        Ok(height)
    }

    /// Return all matching versions, used to derive channel iterations.
    pub fn all_versions(&self, release_unit_id: &str, template: &str) -> Result<Vec<Version>> {
        let tags = self.matching_tags(release_unit_id, template)?;
        let mut versions: Vec<_> = tags.into_values().flatten().collect();
        versions.sort();
        versions.dedup();
        Ok(versions)
    }

    /// Return matching versions excluding tags created at one target commit.
    pub fn all_versions_before(
        &self,
        release_unit_id: &str,
        template: &str,
        excluded_target: gix::ObjectId,
    ) -> Result<Vec<Version>> {
        let mut tags = self.matching_tags(release_unit_id, template)?;
        tags.remove(&excluded_target);
        let mut versions: Vec<_> = tags.into_values().flatten().collect();
        versions.sort();
        versions.dedup();
        Ok(versions)
    }

    fn matching_tags(
        &self,
        release_unit_id: &str,
        template: &str,
    ) -> Result<HashMap<gix::ObjectId, Vec<Version>>> {
        let version_marker = "{version}";
        let rendered = template.replace("{id}", release_unit_id);
        let (prefix, suffix) = rendered.split_once(version_marker).ok_or_else(|| {
            Error::Validation(format!(
                "invalid tag template for release unit {release_unit_id}"
            ))
        })?;

        let repository = self.repository.inner();
        let references = repository
            .references()
            .map_err(|error| Error::Git(format!("failed to read references: {error}")))?;
        let tag_references = references
            .tags()
            .map_err(|error| Error::Git(format!("failed to read tags: {error}")))?;
        let mut matches: HashMap<gix::ObjectId, Vec<Version>> = HashMap::new();

        for mut reference in tag_references.flatten() {
            let name = reference.name().shorten().to_string();
            let Some(version_text) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
            else {
                continue;
            };
            let Ok(version) = Version::parse(version_text) else {
                continue;
            };
            let Ok(target) = reference.peel_to_id() else {
                continue;
            };
            matches.entry(target.detach()).or_default().push(version);
        }

        for versions in matches.values_mut() {
            versions.sort_by(|left, right| right.cmp(left));
        }
        Ok(matches)
    }

    fn walk_to_tag(
        &self,
        matches: &HashMap<gix::ObjectId, Vec<Version>>,
        include_prerelease: bool,
    ) -> Result<(Option<Version>, u64)> {
        let repository = self.repository.inner();
        let mut head = repository
            .head()
            .map_err(|error| Error::Git(format!("failed to read HEAD: {error}")))?;
        let Some(mut current) = head
            .try_peel_to_id()
            .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
            .map(|id| id.detach())
        else {
            return Ok((None, 0));
        };
        let mut height = 0;

        loop {
            if let Some(versions) = matches.get(&current) {
                if let Some(version) = versions
                    .iter()
                    .find(|version| include_prerelease || version.pre.is_empty())
                {
                    return Ok((Some(version.clone()), height));
                }
            }

            let commit = repository
                .find_object(current)
                .map_err(|error| Error::Git(format!("failed to find commit: {error}")))?
                .try_into_commit()
                .map_err(|error| Error::Git(format!("tag target is not a commit: {error}")))?;
            let Some(parent) = commit.parent_ids().next() else {
                return Ok((None, height));
            };
            current = parent.detach();
            height += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_bump_wins() {
        let first = BTreeMap::from([
            ("one".to_owned(), Bump::Patch),
            ("two".to_owned(), Bump::Minor),
        ]);
        let second = BTreeMap::from([
            ("one".to_owned(), Bump::Major),
            ("two".to_owned(), Bump::Patch),
        ]);
        let aggregate = aggregate_bumps([&first, &second]);
        assert_eq!(aggregate["one"], Bump::Major);
        assert_eq!(aggregate["two"], Bump::Minor);
    }

    #[test]
    fn significance_before_1_0() {
        let current = Version::parse("0.4.1").expect("valid version");
        assert_eq!(bump_version(&current, Bump::Major), Version::new(0, 5, 0));
        assert_eq!(bump_version(&current, Bump::Minor), Version::new(0, 4, 2));
        assert_eq!(bump_version(&current, Bump::Patch), Version::new(0, 4, 2));
    }

    #[test]
    fn strict_semver_at_1_0_and_later() {
        let current = Version::parse("1.4.1").expect("valid version");
        assert_eq!(bump_version(&current, Bump::Major), Version::new(2, 0, 0));
        assert_eq!(bump_version(&current, Bump::Minor), Version::new(1, 5, 0));
        assert_eq!(bump_version(&current, Bump::Patch), Version::new(1, 4, 2));
    }

    #[test]
    fn bump_removes_prerelease_and_build_metadata() {
        let current = Version::parse("1.2.3-beta.2+build.1").expect("valid version");
        assert_eq!(bump_version(&current, Bump::Patch), Version::new(1, 2, 4));
    }

    #[test]
    fn dependency_bumps_propagate_only_in_shared_ecosystems() {
        let config = Config::from_yaml(
            r#"
contract: contract-2
release-units:
  library:
    path: library
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  application:
    path: application
    depends-on: [library]
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  unrelated:
    path: unrelated
    depends-on: [library]
    projections:
      - { adapter: cargo, file: Cargo.toml, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  bundle:
    path: bundle
    depends-on: [library]
    projections:
      - { adapter: npm, file: package.json, mode: committed }
      - { adapter: yaml, file: bundle.yml, pointer: /version, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  manual:
    path: manual
    projections:
      - { adapter: yaml, file: manual.yml, pointer: /version, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  manual-index:
    path: manual-index
    depends-on: [manual]
    projections:
      - { adapter: yaml, file: index.yml, pointer: /version, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#,
        )
        .expect("valid config");
        let declared = BTreeMap::from([
            ("library".to_owned(), Bump::Minor),
            ("manual".to_owned(), Bump::Minor),
        ]);
        let effective = effective_bumps(&config, &declared);
        assert_eq!(effective["application"], Bump::Patch);
        assert_eq!(effective["unrelated"], Bump::None);
        // One shared ecosystem adapter is enough, whatever else a unit projects.
        assert_eq!(effective["bundle"], Bump::Patch);
        // Generic adapters carry no dependency ranges, so they never propagate.
        assert_eq!(effective["manual-index"], Bump::None);
    }

    fn group_config(group: &str, suspended: bool) -> Config {
        Config::from_yaml(&format!(
            r#"
contract: contract-2
settings:
  internal-dependency-bump: patch
{group}
release-units:
  package-a:
    path: package-a
    projections: [{{ adapter: npm, file: package.json, mode: committed }}]
    tags:
      primary: {{ role: primary, template: 'package-a@{{version}}' }}
  package-b:
    path: package-b
    {disposition}
    projections: [{{ adapter: npm, file: package.json, mode: committed }}]
    tags:
      primary: {{ role: primary, template: 'package-b@{{version}}' }}
  application:
    path: application
    depends-on: [package-a]
    projections: [{{ adapter: npm, file: package.json, mode: committed }}]
    tags:
      primary: {{ role: primary, template: 'application@{{version}}' }}
"#,
            disposition = if suspended {
                "disposition: suspended"
            } else {
                ""
            }
        ))
        .expect("valid group config")
    }

    fn mixed_current() -> BTreeMap<String, Version> {
        BTreeMap::from([
            ("package-a".to_owned(), Version::new(0, 2, 4)),
            ("package-b".to_owned(), Version::new(0, 4, 1)),
            ("application".to_owned(), Version::new(1, 0, 0)),
        ])
    }

    #[test]
    fn fixed_group_releases_every_member_at_one_version() {
        let config = group_config("fixed: [[package-a, package-b]]", false);
        let declared = BTreeMap::from([
            ("package-a".to_owned(), Bump::Patch),
            ("package-b".to_owned(), Bump::Minor),
        ]);
        let resolved =
            resolve_versions(&config, &declared, &none(), &mixed_current()).expect("resolved");
        assert_eq!(resolved["package-a"].next, Version::new(0, 4, 2));
        assert_eq!(resolved["package-b"].next, Version::new(0, 4, 2));
        assert_eq!(resolved["package-a"].bump, Bump::Minor);
    }

    #[test]
    fn linked_group_releases_only_affected_members_from_highest_current() {
        let config = group_config("linked: [[package-a, package-b]]", false);
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Patch)]);
        let resolved =
            resolve_versions(&config, &declared, &none(), &mixed_current()).expect("resolved");
        assert_eq!(resolved["package-a"].next, Version::new(0, 4, 2));
        assert_eq!(resolved["package-b"].next, Version::new(0, 4, 1));
        assert_eq!(resolved["package-b"].bump, Bump::None);
    }

    #[test]
    fn release_groups_read_bumps_as_significance() {
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Minor)]);
        let resolved = resolve_versions(
            &group_config("linked: [[package-a, package-b]]", false),
            &declared,
            &none(),
            &mixed_current(),
        )
        .expect("resolved");
        assert_eq!(resolved["package-a"].next, Version::new(0, 4, 2));
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Major)]);
        let resolved = resolve_versions(
            &group_config("linked: [[package-a, package-b]]", false),
            &declared,
            &none(),
            &mixed_current(),
        )
        .expect("resolved");
        assert_eq!(resolved["package-a"].next, Version::new(0, 5, 0));
        assert!(!resolved["package-a"].graduates);
    }

    #[test]
    fn dependency_propagation_participates_in_group_fixed_point() {
        let config = group_config("fixed: [[package-b, application]]", false);
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Minor)]);
        let resolved =
            resolve_versions(&config, &declared, &none(), &mixed_current()).expect("resolved");
        assert_eq!(resolved["application"].bump, Bump::Patch);
        assert_eq!(resolved["package-b"].bump, Bump::Patch);
        assert_eq!(resolved["application"].next, Version::new(1, 0, 1));
        assert_eq!(resolved["package-b"].next, Version::new(1, 0, 1));
    }

    #[test]
    fn suspended_group_member_blocks_only_when_required() {
        let fixed = group_config("fixed: [[package-a, package-b]]", true);
        let linked = group_config("linked: [[package-a, package-b]]", true);
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Patch)]);
        assert!(
            resolve_versions(&fixed, &declared, &none(), &mixed_current())
                .expect_err("fixed member blocks")
                .to_string()
                .contains("suspended release unit package-b")
        );
        resolve_versions(&linked, &declared, &none(), &mixed_current())
            .expect("unaffected linked suspended member does not block");
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn graduating(id: &str) -> BTreeSet<String> {
        BTreeSet::from([id.to_owned()])
    }

    #[test]
    fn graduation_releases_exactly_1_0_0() {
        let config = group_config("", false);
        let declared = BTreeMap::from([
            ("package-b".to_owned(), Bump::Major),
            ("package-a".to_owned(), Bump::Patch),
        ]);
        let resolved = resolve_versions(
            &config,
            &declared,
            &graduating("package-b"),
            &mixed_current(),
        )
        .expect("resolved");
        assert_eq!(resolved["package-b"].next, Version::new(1, 0, 0));
        assert!(resolved["package-b"].graduates);
        assert_eq!(resolved["package-b"].bump, Bump::Major);
        assert_eq!(resolved["package-a"].next, Version::new(0, 2, 5));
        assert!(!resolved["package-a"].graduates);
    }

    #[test]
    fn graduation_is_rejected_at_1_0_0_and_later() {
        let config = group_config("", false);
        let declared = BTreeMap::from([("application".to_owned(), Bump::Major)]);
        let error = resolve_versions(
            &config,
            &declared,
            &graduating("application"),
            &mixed_current(),
        )
        .expect_err("graduation at 1.x rejected");
        assert!(error.to_string().contains("graduate is valid only at 0.x"));
    }

    #[test]
    fn graduation_requires_an_effective_major() {
        let config = group_config("", false);
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Minor)]);
        let error = resolve_versions(
            &config,
            &declared,
            &graduating("package-a"),
            &mixed_current(),
        )
        .expect_err("graduation without major rejected");
        assert!(error.to_string().contains("requires bump major"));
    }

    #[test]
    fn one_graduating_member_graduates_a_fixed_group() {
        let config = group_config("fixed: [[package-a, package-b]]", false);
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Major)]);
        let resolved = resolve_versions(
            &config,
            &declared,
            &graduating("package-a"),
            &mixed_current(),
        )
        .expect("resolved");
        for id in ["package-a", "package-b"] {
            assert_eq!(resolved[id].next, Version::new(1, 0, 0));
            assert!(resolved[id].graduates);
        }
    }

    #[test]
    fn linked_group_graduates_only_releasing_members() {
        let config = group_config("linked: [[package-a, package-b]]", false);
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Major)]);
        let resolved = resolve_versions(
            &config,
            &declared,
            &graduating("package-a"),
            &mixed_current(),
        )
        .expect("resolved");
        assert_eq!(resolved["package-a"].next, Version::new(1, 0, 0));
        assert!(resolved["package-a"].graduates);
        assert_eq!(resolved["package-b"].next, Version::new(0, 4, 1));
        assert!(!resolved["package-b"].graduates);
    }

    #[test]
    fn group_graduation_is_rejected_when_the_group_is_past_0_x() {
        let config = group_config("linked: [[package-a, application]]", false);
        let declared = BTreeMap::from([("package-a".to_owned(), Bump::Major)]);
        let error = resolve_versions(
            &config,
            &declared,
            &graduating("package-a"),
            &mixed_current(),
        )
        .expect_err("group at 1.x cannot graduate");
        assert!(error.to_string().contains("graduate is valid only at 0.x"));
    }
}
