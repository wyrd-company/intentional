// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! The `contract-1` interpreter, which only `verify` reaches.
//!
//! Every command but `init` refuses a `contract-1` configuration, and `init`
//! upgrades it. A Release made under `contract-1` still verifies exactly:
//! `verify` rebuilds its plan from the configuration at the release commit
//! under the contract and the pre-1.0 mapping that configuration selected.
//! [`load_released`] is the only way to that interpretation, and only the
//! `verify` module calls it.

use crate::config::{declares_contract_1, upgrade_contract_1_value, Contract1Mapping};
use crate::config::{Config, CONFIG_PATH, CONTRACT_1};
use crate::error::{Error, Result};
use crate::model::Bump;
use crate::version::bump_version;
use semver::Version;
use std::path::Path;

/// How a plan turns bump names into versions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Interpretation {
    /// The current contract, `contract-2`.
    #[default]
    Current,
    /// A `contract-1` configuration, which only `verify` reads.
    Contract1(Contract1),
}

/// The pre-1.0 mapping of one `contract-1` configuration. Its field is
/// private, so that only [`load_released`] builds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Contract1 {
    mapping: Contract1Mapping,
}

impl Interpretation {
    /// Apply one bump to `current`.
    ///
    /// `contract-1` with mapping `compatibility` read bump names as change
    /// significance, as `contract-2` does. With mapping `component`, a bump
    /// name named the version component to increment, before 1.0.0 too.
    pub(crate) fn bump(self, current: &Version, bump: Bump) -> Version {
        match self {
            Self::Contract1(Contract1 {
                mapping: Contract1Mapping::Component,
            }) => component_bump(current, bump),
            Self::Current
            | Self::Contract1(Contract1 {
                mapping: Contract1Mapping::Compatibility,
            }) => bump_version(current, bump),
        }
    }

    /// Refuse graduation, which `contract-1` did not have.
    pub(crate) fn check_graduation(self, graduating: usize) -> Result<()> {
        if graduating > 0 && self != Self::Current {
            return Err(Error::Validation(
                "contract-1 has no graduation; an intent that graduates belongs to contract-2"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

/// Load the configuration at the commit a Release was planned from, with the
/// interpretation that Release was made under.
///
/// A `contract-1` configuration loads as its `init` upgrade would, keeps
/// `contract: contract-1` so that the rebuilt plan carries the contract its
/// tag records name, and keeps its pre-1.0 mapping in the interpretation.
pub(crate) fn load_released(root: &Path) -> Result<(Config, Interpretation)> {
    let path = root.join(CONFIG_PATH);
    let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
    let mut value: serde_yaml::Value = serde_yaml::from_str(&text)?;
    if !declares_contract_1(&value) {
        return Ok((Config::from_yaml(&text)?, Interpretation::Current));
    }
    let mapping = upgrade_contract_1_value(&mut value)?;
    let mut config: Config = serde_yaml::from_value(value)?;
    config.validate()?;
    config.contract = CONTRACT_1.to_owned();
    Ok((config, Interpretation::Contract1(Contract1 { mapping })))
}

/// `contract-1` component mapping: increment the named component.
fn component_bump(current: &Version, bump: Bump) -> Version {
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

#[cfg(test)]
mod tests {
    use super::*;

    const COMPONENT: Interpretation = Interpretation::Contract1(Contract1 {
        mapping: Contract1Mapping::Component,
    });
    const COMPATIBILITY: Interpretation = Interpretation::Contract1(Contract1 {
        mapping: Contract1Mapping::Compatibility,
    });

    #[test]
    fn component_mapping_increments_the_named_component_before_1_0_0() {
        let current = Version::new(0, 4, 1);
        assert_eq!(COMPONENT.bump(&current, Bump::Major), Version::new(1, 0, 0));
        assert_eq!(COMPONENT.bump(&current, Bump::Minor), Version::new(0, 5, 0));
        assert_eq!(COMPONENT.bump(&current, Bump::Patch), Version::new(0, 4, 2));
    }

    #[test]
    fn compatibility_mapping_is_the_current_interpretation() {
        for current in [Version::new(0, 4, 1), Version::new(1, 4, 1)] {
            for bump in [Bump::Major, Bump::Minor, Bump::Patch, Bump::None] {
                assert_eq!(
                    COMPATIBILITY.bump(&current, bump),
                    Interpretation::Current.bump(&current, bump)
                );
            }
        }
    }

    #[test]
    fn both_mappings_agree_from_1_0_0() {
        let current = Version::new(1, 4, 1);
        for bump in [Bump::Major, Bump::Minor, Bump::Patch] {
            assert_eq!(
                COMPONENT.bump(&current, bump),
                COMPATIBILITY.bump(&current, bump)
            );
        }
    }

    #[test]
    fn contract_1_refuses_graduation() {
        assert!(COMPONENT.check_graduation(1).is_err());
        assert!(COMPONENT.check_graduation(0).is_ok());
        assert!(Interpretation::Current.check_graduation(1).is_ok());
    }
}
