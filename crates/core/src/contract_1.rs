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

use crate::adapters::format::json_pointer;
use crate::config::{declares_contract_1, upgrade_contract_1_value, Contract1Mapping};
use crate::config::{Config, CONFIG_PATH, CONTRACT_1};
use crate::error::{Error, Result};
use crate::model::{Adapter, Bump};
use crate::version::{bump_version, component_bump};
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
        self.bump_fn()(current, bump)
    }

    /// The bump function for [`crate::version::resolve_versions_with`].
    pub(crate) fn bump_fn(self) -> fn(&Version, Bump) -> Version {
        match self {
            Self::Contract1(Contract1 {
                mapping: Contract1Mapping::Component,
            }) => component_bump,
            Self::Current
            | Self::Contract1(Contract1 {
                mapping: Contract1Mapping::Compatibility,
            }) => bump_version,
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
    config.validate_released()?;
    released_json_pointers(root, &mut config);
    config.contract = CONTRACT_1.to_owned();
    Ok((config, Interpretation::Contract1(Contract1 { mapping })))
}

/// Spell each JSON array index the way the current reader selects it.
///
/// Released 0.2.0 read an array segment as any text `usize` parses, so
/// `01` and `+1` selected element 1. Since 0.2.1 only the canonical `1`
/// does. The array structure along a pointer is the same in every tree of a
/// release, because a release writes only the scalar at its end, so the
/// pointer is resolved once against the file at `root`. An unreadable file
/// keeps its pointer, and the rebuild reports it as before.
fn released_json_pointers(root: &Path, config: &mut Config) {
    for unit in config.release_units.values_mut() {
        let unit_path = unit.path.clone();
        for projection in &mut unit.projections {
            if projection.adapter != Adapter::Json {
                continue;
            }
            let Some(pointer) = projection.pointer.as_deref() else {
                continue;
            };
            let path = root.join(projection.file.workspace_path(&unit_path));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if let Some(canonical) = canonical_json_pointer(&document, pointer) {
                projection.pointer = Some(canonical);
            }
        }
    }
}

/// `pointer` with every array segment respelled as its canonical index, or
/// `None` when it does not start with `/`.
fn canonical_json_pointer(document: &serde_json::Value, pointer: &str) -> Option<String> {
    let segments = json_pointer(pointer).ok()?;
    let mut node = Some(document);
    let mut canonical = String::new();
    for segment in segments {
        let (text, next) = match node {
            Some(serde_json::Value::Array(items)) => match segment.parse::<usize>() {
                Ok(index) => (index.to_string(), items.get(index)),
                Err(_) => (segment, None),
            },
            Some(serde_json::Value::Object(members)) => {
                let next = members.get(&segment);
                (segment, next)
            }
            _ => (segment, None),
        };
        canonical.push('/');
        canonical.push_str(&text.replace('~', "~0").replace('/', "~1"));
        node = next;
    }
    Some(canonical)
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
    fn released_array_indexes_take_their_canonical_spelling() {
        let document: serde_json::Value =
            serde_json::from_str(r#"{"versions":["a","b"],"01":{"x":[1,2]}}"#).unwrap();
        for (released, canonical) in [
            ("/versions/1", "/versions/1"),
            ("/versions/01", "/versions/1"),
            ("/versions/+1", "/versions/1"),
            ("/01/x/001", "/01/x/1"),
            ("/missing/01", "/missing/01"),
        ] {
            assert_eq!(
                canonical_json_pointer(&document, released).as_deref(),
                Some(canonical),
                "{released}"
            );
        }
        assert_eq!(canonical_json_pointer(&document, "versions"), None);
    }

    #[test]
    fn contract_1_refuses_graduation() {
        assert!(COMPONENT.check_graduation(1).is_err());
        assert!(COMPONENT.check_graduation(0).is_ok());
        assert!(Interpretation::Current.check_graduation(1).is_ok());
    }
}
