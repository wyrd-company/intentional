// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Lightweight Git tag creation for applied releases.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::Bump;
use crate::version::{bump_version, VersionRepository};
use semver::{Prerelease, Version};
use std::collections::BTreeSet;
use std::path::Path;

/// Planned lightweight tags for an applied release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagResult {
    /// Tag names in deterministic order.
    pub tags: Vec<String>,
}

impl TagResult {
    /// Recover the applied release and plan its tags.
    pub fn build(root: &Path, channel: Option<&str>) -> Result<Self> {
        let config = Config::load(root)?;
        let mut tags = BTreeSet::new();
        let versions = VersionRepository::discover(root)?;
        let mut max_bump = Bump::None;
        for (id, package) in &config.packages {
            let changelog_path = root.join(&package.path).join("CHANGELOG.md");
            let Ok(changelog) = std::fs::read_to_string(&changelog_path) else {
                continue;
            };
            let Some(version) = leading_changelog_version(&changelog)? else {
                continue;
            };
            let matches_channel = match channel {
                Some(channel) => channel_iteration(&version, channel).is_some(),
                None => version.pre.is_empty(),
            };
            if !matches_channel {
                continue;
            }
            let current = versions.current_version(id, &package.tag)?;
            if version <= current {
                continue;
            }
            max_bump = max_bump.max(infer_bump(&current, &version));
            tags.insert(
                package
                    .tag
                    .replace("{id}", id)
                    .replace("{version}", &version.to_string()),
            );
        }
        if config.settings.global_tag && max_bump != Bump::None {
            let current = versions.current_version("", "{version}")?;
            let base = bump_version(&current, max_bump);
            let version = match channel {
                Some(channel) => global_channel_version(&versions, &base, channel)?,
                None => base,
            };
            tags.insert(version.to_string());
        }
        if tags.is_empty() {
            return Err(Error::Validation(
                "no applied release is available to tag".to_owned(),
            ));
        }
        let repository = gix::discover(root)
            .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
        for tag in &tags {
            let reference_name = format!("refs/tags/{tag}");
            if repository
                .try_find_reference(reference_name.as_str())
                .map_err(|error| Error::Git(format!("failed to inspect tag {tag}: {error}")))?
                .is_some()
            {
                return Err(Error::Validation(format!("tag {tag} already exists")));
            }
        }
        Ok(Self {
            tags: tags.into_iter().collect(),
        })
    }

    /// Human-readable operations printed identically for dry and real runs.
    pub fn operations(&self) -> Vec<String> {
        self.tags
            .iter()
            .map(|tag| format!("create tag {tag}"))
            .collect()
    }

    /// Create lightweight tags at HEAD unless `dry_run` is enabled.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        let repository = gix::discover(root)
            .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
        let head = repository
            .head_id()
            .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
            .detach();
        for tag in &self.tags {
            repository
                .tag_reference(
                    tag,
                    head,
                    gix::refs::transaction::PreviousValue::MustNotExist,
                )
                .map_err(|error| Error::Git(format!("failed to create tag {tag}: {error}")))?;
        }
        Ok(())
    }
}

fn leading_changelog_version(changelog: &str) -> Result<Option<Version>> {
    let Some(heading) = changelog.lines().find(|line| line.starts_with("## ")) else {
        return Ok(None);
    };
    Version::parse(heading.trim_start_matches("## ").trim())
        .map(Some)
        .map_err(Error::from)
}

fn channel_iteration(version: &Version, channel: &str) -> Option<u64> {
    let (name, iteration) = version.pre.as_str().split_once('.')?;
    (name == channel).then(|| iteration.parse().ok()).flatten()
}

fn infer_bump(current: &Version, applied: &Version) -> Bump {
    if applied.major > current.major {
        Bump::Major
    } else if applied.minor > current.minor {
        Bump::Minor
    } else if applied.patch > current.patch {
        Bump::Patch
    } else {
        Bump::None
    }
}

fn global_channel_version(
    repository: &VersionRepository,
    base: &Version,
    channel: &str,
) -> Result<Version> {
    let iteration = repository
        .all_versions("", "{version}")?
        .into_iter()
        .filter(|version| {
            version.major == base.major
                && version.minor == base.minor
                && version.patch == base.patch
        })
        .filter_map(|version| channel_iteration(&version, channel))
        .max()
        .unwrap_or(0)
        + 1;
    let mut version = base.clone();
    version.pre = Prerelease::new(&format!("{channel}.{iteration}"))?;
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_leading_release_section() {
        let changelog = "# Changelog\n\n## 2.1.0\n\nNotes.\n\n## 2.0.0\n";
        assert_eq!(
            leading_changelog_version(changelog).expect("valid changelog"),
            Some(Version::new(2, 1, 0))
        );
    }

    #[test]
    fn infers_highest_semantic_bump() {
        assert_eq!(
            infer_bump(&Version::new(4, 2, 1), &Version::new(4, 3, 0)),
            Bump::Minor
        );
        assert_eq!(
            infer_bump(&Version::new(4, 2, 1), &Version::new(5, 0, 0)),
            Bump::Major
        );
    }
}
