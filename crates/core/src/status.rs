// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace status computation.

use crate::config::Config;
use crate::error::Result;
use crate::intent::Intent;
use crate::model::Bump;
use crate::version::{aggregate_bumps, effective_bumps, PackageVersion, VersionRepository};
use semver::Version;
use std::path::Path;

/// Version status for one logical package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageStatus {
    /// Logical package id.
    pub id: String,
    /// Tag-derived current version.
    pub current: Version,
    /// Intent-derived next version.
    pub next: Version,
    /// Effective bump after dependency propagation.
    pub bump: Bump,
}

/// Complete read-only workspace status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStatus {
    /// Pending intent ids.
    pub intents: Vec<String>,
    /// Package versions in id order.
    pub packages: Vec<PackageStatus>,
}

impl WorkspaceStatus {
    /// Load config, intents, Git tags, and compute effective versions.
    pub fn load(root: &Path) -> Result<Self> {
        let config = Config::load(root)?;
        let intents = Intent::load_all(root, &config)?;
        Self::compute(root, &config, &intents)
    }

    /// Compute status from already-loaded inputs.
    pub fn compute(root: &Path, config: &Config, intents: &[Intent]) -> Result<Self> {
        let repository = VersionRepository::discover(root)?;
        let declared = aggregate_bumps(intents.iter().map(|intent| &intent.packages));
        let bumps = effective_bumps(config, &declared);
        let mut packages = Vec::with_capacity(config.packages.len());
        for (id, package) in &config.packages {
            let current = repository.current_version(id, &package.tag)?;
            let versions = PackageVersion::new(current, bumps[id]);
            packages.push(PackageStatus {
                id: id.clone(),
                current: versions.current,
                next: versions.next,
                bump: versions.bump,
            });
        }
        Ok(Self {
            intents: intents.iter().map(|intent| intent.id.clone()).collect(),
            packages,
        })
    }
}
