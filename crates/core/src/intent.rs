// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Intent file parsing, validation, and authoring.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::Bump;
use rand::seq::SliceRandom;
use rand::Rng;
use std::collections::BTreeMap;
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
    /// Package bump claims.
    pub packages: BTreeMap<String, Bump>,
    /// Markdown changelog prose.
    pub message: String,
    /// Source file.
    pub path: PathBuf,
}

/// Inputs used to author an intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentDraft {
    /// Package bump claims.
    pub packages: BTreeMap<String, Bump>,
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
        let packages: BTreeMap<String, Bump> = serde_yaml::from_str(frontmatter)?;
        validate_draft(&packages, message, config)?;
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                Error::Validation(format!("invalid intent filename {}", path.display()))
            })?
            .to_owned();
        Ok(Self {
            id,
            packages,
            message: message.trim().to_owned(),
            path: path.to_owned(),
        })
    }
}

impl IntentDraft {
    /// Validate and render a new intent with a generated memorable slug.
    pub fn plan(self, root: &Path, config: &Config) -> Result<IntentWrite> {
        validate_draft(&self.packages, &self.message, config)?;
        let mut rng = rand::thread_rng();
        let adjective = ADJECTIVES
            .choose(&mut rng)
            .expect("adjectives are non-empty");
        let noun = NOUNS.choose(&mut rng).expect("nouns are non-empty");
        let suffix: u32 = rng.gen_range(0..=0xffff);
        let slug = format!("{adjective}-{noun}-{suffix:04x}");
        let relative = PathBuf::from(INTENTS_PATH).join(format!("{slug}.md"));
        let path = root.join(&relative);
        if path.exists() {
            return Err(Error::Validation(format!(
                "generated intent path already exists: {}",
                relative.display()
            )));
        }
        let yaml = serde_yaml::to_string(&self.packages)?;
        let contents = format!("---\n{yaml}---\n\n{}\n", self.message.trim());
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

fn validate_draft(packages: &BTreeMap<String, Bump>, message: &str, config: &Config) -> Result<()> {
    if packages.is_empty() {
        return Err(Error::Validation(
            "intent must reference at least one package".to_owned(),
        ));
    }
    if message.trim().is_empty() {
        return Err(Error::Validation(
            "intent changelog message must not be empty".to_owned(),
        ));
    }
    for (package, bump) in packages {
        if !config.packages.contains_key(package) {
            return Err(Error::Validation(format!(
                "intent references unknown package {package}"
            )));
        }
        if *bump == Bump::None {
            return Err(Error::Validation(format!(
                "intent package {package} must declare major, minor, or patch"
            )));
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
contract: contract-1
packages:
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
            packages: BTreeMap::from([("library".to_owned(), Bump::Minor)]),
            message: "Add a useful capability.".to_owned(),
        };
        let write = draft.plan(Path::new("."), &config()).expect("planned");
        assert!(write.path.to_string_lossy().starts_with(INTENTS_PATH));
        assert!(write.contents.starts_with("---\nlibrary: minor\n---"));
        assert!(write.contents.contains("Add a useful capability."));
    }

    #[test]
    fn rejects_unknown_package() {
        let draft = IntentDraft {
            packages: BTreeMap::from([("missing".to_owned(), Bump::Patch)]),
            message: "Correct a defect.".to_owned(),
        };
        let error = draft
            .plan(Path::new("."), &config())
            .expect_err("unknown package rejected");
        assert!(error.to_string().contains("unknown package missing"));
    }
}
