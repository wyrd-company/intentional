// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Lightweight Git tag creation for applied releases.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::plan::ReleasePlan;
use crate::version::VersionRepository;
use semver::Version;
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
        if let Some(channel) = channel {
            let plan = ReleasePlan::build(root, Some(channel))?;
            for package in &plan.packages {
                tags.extend(package.tags.iter().cloned());
            }
            if let Some(global) = plan.global_tag {
                tags.insert(global);
            }
        } else {
            let versions = VersionRepository::discover(root)?;
            let mut applied = Vec::new();
            for (id, package) in &config.packages {
                let changelog_path = root.join(&package.path).join("CHANGELOG.md");
                let Ok(changelog) = std::fs::read_to_string(&changelog_path) else {
                    continue;
                };
                let Some(version) = leading_changelog_version(&changelog)? else {
                    continue;
                };
                if !version.pre.is_empty() {
                    continue;
                }
                let current = versions.current_version(id, &package.tag)?;
                if version <= current {
                    continue;
                }
                tags.insert(
                    package
                        .tag
                        .replace("{id}", id)
                        .replace("{version}", &version.to_string()),
                );
                applied.push(version);
            }
            if config.settings.global_tag {
                if let Some(version) = applied.into_iter().max() {
                    tags.insert(version.to_string());
                }
            }
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
}
