// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Intent file parsing, validation, and authoring.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::Bump;
use rand::seq::IndexedRandom;
use rand::RngExt;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Directory containing pending intent files.
pub const INTENTS_PATH: &str = ".intentional/intents";

const ADJECTIVES: &[&str] = &[
    "amber", "brisk", "calm", "clear", "gentle", "lucky", "quiet", "swift",
];
const NOUNS: &[&str] = &[
    "badger", "comet", "falcon", "lantern", "otter", "panda", "river", "willow",
];

/// Parsed change intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Stable identifier derived from the filename stem.
    pub id: String,
    /// Release-unit bump claims.
    pub release_units: BTreeMap<String, Bump>,
    /// Release units this intent graduates to `1.0.0`.
    pub graduates: BTreeSet<String>,
    /// Markdown changelog prose.
    pub message: String,
    /// Source file.
    pub path: PathBuf,
}

/// Inputs used to author an intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentDraft {
    /// Release-unit bump claims.
    pub release_units: BTreeMap<String, Bump>,
    /// Release units this intent graduates to `1.0.0`.
    pub graduates: BTreeSet<String>,
    /// Markdown changelog prose.
    pub message: String,
}

/// Planned intent file write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentWrite {
    /// Target path relative to the workspace.
    pub path: PathBuf,
    /// Complete file contents.
    pub contents: String,
}

impl Intent {
    /// Load every pending intent in stable filename order.
    pub fn load_all(root: &Path, config: &Config) -> Result<Vec<Self>> {
        let directory = root.join(INTENTS_PATH);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut paths = std::fs::read_dir(&directory)
            .map_err(|error| Error::io(&directory, error))?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| Self::load(&path, config))
            .collect()
    }

    /// Load and validate one intent file.
    pub fn load(path: &Path, config: &Config) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        Self::parse(path, &text, config)
    }

    /// Parse and validate one intent from supplied contents.
    pub fn parse(path: &Path, text: &str, config: &Config) -> Result<Self> {
        let (frontmatter, message) = split_frontmatter(text)?;
        let (release_units, graduates) = parse_claims(frontmatter)?;
        validate_draft(&release_units, &graduates, message, config)?;
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                Error::Validation(format!("invalid intent filename {}", path.display()))
            })?
            .to_owned();
        Ok(Self {
            id,
            release_units,
            graduates,
            message: message.trim().to_owned(),
            path: path.to_owned(),
        })
    }
}

impl IntentDraft {
    /// Validate and render a new intent with a generated memorable slug.
    pub fn plan(self, root: &Path, config: &Config) -> Result<IntentWrite> {
        validate_draft(&self.release_units, &self.graduates, &self.message, config)?;
        let mut rng = rand::rng();
        let adjective = ADJECTIVES
            .choose(&mut rng)
            .expect("adjectives are non-empty");
        let noun = NOUNS.choose(&mut rng).expect("nouns are non-empty");
        let suffix: u32 = rng.random_range(0..=0xffff);
        let slug = format!("{adjective}-{noun}-{suffix:04x}");
        let relative = PathBuf::from(INTENTS_PATH).join(format!("{slug}.md"));
        let path = root.join(&relative);
        if path.exists() {
            return Err(Error::Validation(format!(
                "generated intent path already exists: {}",
                relative.display()
            )));
        }
        let contents = render_intent(&self.release_units, &self.graduates, &self.message)?;
        Ok(IntentWrite {
            path: relative,
            contents,
        })
    }
}

impl IntentWrite {
    /// Materialize this write unless `dry_run` is set.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        let path = root.join(&self.path);
        let parent = path.parent().expect("intent path has a parent");
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        std::fs::write(&path, &self.contents).map_err(|error| Error::io(&path, error))
    }
}

/// Render intent file contents. A graduating release unit uses the long
/// form `{ bump: major, graduate: true }`; every other one uses the short
/// form `id: bump`.
pub(crate) fn render_intent(
    release_units: &BTreeMap<String, Bump>,
    graduates: &BTreeSet<String>,
    message: &str,
) -> Result<String> {
    let mut frontmatter = serde_yaml::Mapping::new();
    for (id, bump) in release_units {
        let value = if graduates.contains(id) {
            let mut claim = serde_yaml::Mapping::new();
            claim.insert("bump".into(), serde_yaml::to_value(bump)?);
            claim.insert("graduate".into(), true.into());
            serde_yaml::Value::Mapping(claim)
        } else {
            serde_yaml::to_value(bump)?
        };
        frontmatter.insert(id.as_str().into(), value);
    }
    let yaml = serde_yaml::to_string(&frontmatter)?;
    Ok(format!("---\n{yaml}---\n\n{}\n", message.trim()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LongClaim {
    bump: Bump,
    #[serde(default)]
    graduate: bool,
}

/// Parse frontmatter claims. Each value is a bump name, or a mapping with
/// `bump` and an optional `graduate`.
fn parse_claims(frontmatter: &str) -> Result<(BTreeMap<String, Bump>, BTreeSet<String>)> {
    let claims: BTreeMap<String, serde_yaml::Value> = serde_yaml::from_str(frontmatter)?;
    let mut release_units = BTreeMap::new();
    let mut graduates = BTreeSet::new();
    for (id, value) in claims {
        let bump = match value {
            serde_yaml::Value::String(_) => serde_yaml::from_value(value)?,
            serde_yaml::Value::Mapping(_) => {
                let claim: LongClaim = serde_yaml::from_value(value)?;
                if claim.graduate {
                    graduates.insert(id.clone());
                }
                claim.bump
            }
            _ => {
                return Err(Error::Validation(format!(
                    "intent release unit {id} must declare a bump name or a mapping with bump and graduate"
                )))
            }
        };
        release_units.insert(id, bump);
    }
    Ok((release_units, graduates))
}

fn validate_draft(
    release_units: &BTreeMap<String, Bump>,
    graduates: &BTreeSet<String>,
    message: &str,
    config: &Config,
) -> Result<()> {
    if release_units.is_empty() {
        return Err(Error::Validation(
            "intent must reference at least one release unit".to_owned(),
        ));
    }
    if message.trim().is_empty() {
        return Err(Error::Validation(
            "intent changelog message must not be empty".to_owned(),
        ));
    }
    for (release_unit, bump) in release_units {
        if !config.release_units.contains_key(release_unit) {
            return Err(Error::Validation(format!(
                "intent references unknown release unit {release_unit}"
            )));
        }
        if *bump == Bump::None {
            return Err(Error::Validation(format!(
                "intent release unit {release_unit} must declare major, minor, or patch"
            )));
        }
    }
    for release_unit in graduates {
        match release_units.get(release_unit) {
            Some(Bump::Major) => {}
            Some(_) => {
                return Err(Error::Validation(format!(
                    "intent release unit {release_unit} sets graduate, which requires bump major"
                )))
            }
            None => {
                return Err(Error::Validation(format!(
                    "intent graduates release unit {release_unit}, which it does not bump"
                )))
            }
        }
    }
    Ok(())
}

fn split_frontmatter(text: &str) -> Result<(&str, &str)> {
    let rest = text.strip_prefix("---\n").ok_or_else(|| {
        Error::Validation("intent must start with YAML frontmatter delimiter".to_owned())
    })?;
    let (frontmatter, message) = rest.split_once("\n---\n").ok_or_else(|| {
        Error::Validation("intent frontmatter is missing its closing delimiter".to_owned())
    })?;
    Ok((frontmatter, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::from_yaml(
            r#"
contract: contract-2
release-units:
  library:
    path: .
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#,
        )
        .expect("valid config")
    }

    #[test]
    fn plans_direct_frontmatter_mapping() {
        let draft = IntentDraft {
            release_units: BTreeMap::from([("library".to_owned(), Bump::Minor)]),
            graduates: BTreeSet::new(),
            message: "Add a useful capability.".to_owned(),
        };
        let write = draft.plan(Path::new("."), &config()).expect("planned");
        assert!(write.path.to_string_lossy().starts_with(INTENTS_PATH));
        assert!(write.contents.starts_with("---\nlibrary: minor\n---"));
        assert!(write.contents.contains("Add a useful capability."));
    }

    #[test]
    fn rejects_unknown_release_unit() {
        let draft = IntentDraft {
            release_units: BTreeMap::from([("missing".to_owned(), Bump::Patch)]),
            graduates: BTreeSet::new(),
            message: "Correct a defect.".to_owned(),
        };
        let error = draft
            .plan(Path::new("."), &config())
            .expect_err("unknown release unit rejected");
        assert!(error.to_string().contains("unknown release unit missing"));
    }

    #[test]
    fn graduating_release_unit_round_trips_in_long_form() {
        let draft = IntentDraft {
            release_units: BTreeMap::from([("library".to_owned(), Bump::Major)]),
            graduates: BTreeSet::from(["library".to_owned()]),
            message: "Declare the stable interface.".to_owned(),
        };
        let write = draft.plan(Path::new("."), &config()).expect("planned");
        assert!(write
            .contents
            .starts_with("---\nlibrary:\n  bump: major\n  graduate: true\n---"));
        let intent =
            Intent::parse(Path::new("x.md"), &write.contents, &config()).expect("parsed back");
        assert_eq!(intent.release_units["library"], Bump::Major);
        assert!(intent.graduates.contains("library"));
    }

    #[test]
    fn long_form_without_graduate_is_an_ordinary_bump() {
        let intent = Intent::parse(
            Path::new("x.md"),
            "---\nlibrary: { bump: minor }\n---\n\nAdd a capability.\n",
            &config(),
        )
        .expect("parsed");
        assert_eq!(intent.release_units["library"], Bump::Minor);
        assert!(intent.graduates.is_empty());
    }

    #[test]
    fn graduate_requires_major() {
        let error = Intent::parse(
            Path::new("x.md"),
            "---\nlibrary: { bump: minor, graduate: true }\n---\n\nAdd a capability.\n",
            &config(),
        )
        .expect_err("graduate with minor rejected");
        assert!(error.to_string().contains("requires bump major"));
    }
}
