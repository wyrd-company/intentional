// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Tag-derived current versions and intent-derived next versions.

use crate::error::{Error, Result};
use crate::model::Bump;
use semver::Version;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Current and next version of a logical package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVersion {
    /// Latest final version tag on the first-parent history.
    pub current: Version,
    /// Current version after the aggregate intent bump.
    pub next: Version,
    /// Aggregate intent bump.
    pub bump: Bump,
}

impl PackageVersion {
    /// Compute package versions from a current tag and aggregate bump.
    pub fn new(current: Version, bump: Bump) -> Self {
        let next = bump_version(&current, bump);
        Self {
            current,
            next,
            bump,
        }
    }
}

/// Aggregate intent bumps by taking the maximum significance for each package.
pub fn aggregate_bumps<'a>(
    intents: impl IntoIterator<Item = &'a BTreeMap<String, Bump>>,
) -> BTreeMap<String, Bump> {
    let mut aggregate: BTreeMap<String, Bump> = BTreeMap::new();
    for intent in intents {
        for (package, bump) in intent {
            aggregate
                .entry(package.clone())
                .and_modify(|existing| *existing = (*existing).max(*bump))
                .or_insert(*bump);
        }
    }
    aggregate
}

/// Apply strict SemVer bump semantics, including pre-1.0 versions.
pub fn bump_version(current: &Version, bump: Bump) -> Version {
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

/// Read package versions and heights from a Git repository using tagver traversal semantics.
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

    /// Find the latest final SemVer matching a package tag template.
    pub fn current_version(&self, package_id: &str, template: &str) -> Result<Version> {
        let matches = self.matching_tags(package_id, template)?;
        let (version, _) = self.walk_to_tag(&matches, false)?;
        Ok(version.unwrap_or_else(|| Version::new(0, 0, 0)))
    }

    /// Count commits on the first-parent chain since the latest matching tag.
    pub fn height(&self, package_id: &str, template: &str) -> Result<u64> {
        let matches = self.matching_tags(package_id, template)?;
        let (_, height) = self.walk_to_tag(&matches, true)?;
        Ok(height)
    }

    /// Return all matching versions, used to derive channel iterations.
    pub fn all_versions(&self, package_id: &str, template: &str) -> Result<Vec<Version>> {
        let tags = self.matching_tags(package_id, template)?;
        let mut versions: Vec<_> = tags.into_values().flatten().collect();
        versions.sort();
        versions.dedup();
        Ok(versions)
    }

    fn matching_tags(
        &self,
        package_id: &str,
        template: &str,
    ) -> Result<HashMap<gix::ObjectId, Vec<Version>>> {
        let version_marker = "{version}";
        let rendered = template.replace("{id}", package_id);
        let (prefix, suffix) = rendered.split_once(version_marker).ok_or_else(|| {
            Error::Validation(format!("invalid tag template for package {package_id}"))
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
    fn strict_pre_1_0_mapping() {
        let current = Version::parse("0.4.7").expect("valid version");
        assert_eq!(bump_version(&current, Bump::Major), Version::new(1, 0, 0));
        assert_eq!(bump_version(&current, Bump::Minor), Version::new(0, 5, 0));
        assert_eq!(bump_version(&current, Bump::Patch), Version::new(0, 4, 8));
    }

    #[test]
    fn bump_removes_prerelease_and_build_metadata() {
        let current = Version::parse("1.2.3-beta.2+build.1").expect("valid version");
        assert_eq!(bump_version(&current, Bump::Patch), Version::new(1, 2, 4));
    }
}
