// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Ownership of the version fields that projections write.

use crate::adapters::format::{json_pointer, key_pointer};
use crate::config::{Config, Projection, ReleaseUnitConfig};
use crate::error::{Error, Result};
use crate::model::{Adapter, ProjectionMode};
use crate::workspace_files::{contained, FileIdentity};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The field in a file that one projection reads and writes.
///
/// Generic pointers compare by their parsed segments, so `a.b` and `/a/b`
/// are one field. An ecosystem adapter writes one fixed field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ProjectionField {
    /// A parsed JSON pointer or TOML/YAML key path.
    Keys(Vec<String>),
    /// A pointer that does not parse; it compares by its text.
    Unparsed(String),
    /// The `<Version>` element of an MSBuild project.
    MsbuildVersion,
    /// The `module` path of a Go module file.
    GoModule,
}

impl ProjectionField {
    pub(crate) fn of(projection: &Projection) -> Self {
        let keys = |keys: &[&str]| Self::Keys(keys.iter().map(|key| (*key).to_owned()).collect());
        let pointer = projection.pointer.as_deref().unwrap_or_default();
        let parsed = match projection.adapter {
            Adapter::Npm | Adapter::Pub => return keys(&["version"]),
            Adapter::Cargo => return keys(&["package", "version"]),
            Adapter::Python => return keys(&["project", "version"]),
            Adapter::Msbuild => return Self::MsbuildVersion,
            Adapter::Go => return Self::GoModule,
            Adapter::Json => json_pointer(pointer),
            Adapter::Toml | Adapter::Yaml => key_pointer(pointer),
        };
        match parsed {
            Ok(keys) => Self::Keys(keys),
            Err(_) => Self::Unparsed(pointer.to_owned()),
        }
    }
}

impl fmt::Display for ProjectionField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keys(keys) => {
                for key in keys {
                    write!(formatter, "/{}", key.replace('~', "~0").replace('/', "~1"))?;
                }
                Ok(())
            }
            Self::Unparsed(pointer) => formatter.write_str(pointer),
            Self::MsbuildVersion => formatter.write_str("<Version>"),
            Self::GoModule => formatter.write_str("the module path"),
        }
    }
}

/// Refuse two projections that write one field of one file.
///
/// Files compare by their resolved workspace path, so `/docs/x.yml` on any
/// unit and `docs/x.yml` on a unit at `.` are one file. Projections with
/// mode `none` neither read nor write, so they do not count.
pub(crate) fn check_ownership(release_units: &BTreeMap<String, ReleaseUnitConfig>) -> Result<()> {
    let mut owners = BTreeMap::<(PathBuf, ProjectionField), &str>::new();
    for (id, release_unit) in release_units {
        for projection in &release_unit.projections {
            if projection.mode == ProjectionMode::None {
                continue;
            }
            let path = projection.file.workspace_path(&release_unit.path);
            let field = ProjectionField::of(projection);
            if let Some(owner) = owners.get(&(path.clone(), field.clone())) {
                return Err(if *owner == id.as_str() {
                    Error::Validation(format!(
                        "release unit {id} repeats projection {}",
                        projection.file
                    ))
                } else {
                    Error::Validation(format!(
                        "release units {owner} and {id} both project {field} in {}",
                        path.display()
                    ))
                });
            }
            owners.insert((path, field), id);
        }
    }
    Ok(())
}

/// Refuse two projections that write one field of one physical file.
///
/// Config load compares paths as text, so it cannot see two paths that
/// reach one file through symlinks, or through letter case on a
/// filesystem that ignores it. Commands that edit or read projections run
/// this check against the filesystem.
pub(crate) fn check_physical_ownership(root: &Path, config: &Config) -> Result<()> {
    let mut owners = Vec::<(FileIdentity, PathBuf, ProjectionField, &str)>::new();
    for (id, release_unit) in &config.release_units {
        for projection in &release_unit.projections {
            if projection.mode == ProjectionMode::None {
                continue;
            }
            let path = projection.file.workspace_path(&release_unit.path);
            let identity = FileIdentity::of(&contained(root, &path)?)?;
            let field = ProjectionField::of(projection);
            if let Some((_, other, _, owner)) =
                owners.iter().find(|(known, other, known_field, _)| {
                    *known == identity && *known_field == field && *other != path
                })
            {
                let owners = if *owner == id.as_str() {
                    format!("release unit {id} projects {field} twice")
                } else {
                    format!("release units {owner} and {id} both project {field}")
                };
                return Err(Error::Validation(format!(
                    "{owners} in one file: {} and {} are the same file",
                    other.display(),
                    path.display()
                )));
            }
            owners.push((identity, path, field, id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn physical_check_refuses_one_unit_reaching_one_field_twice() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path();
        fs::create_dir(root.join("a")).expect("unit directory");
        fs::write(root.join("a/meta.yml"), "version: 1.0.0\n").expect("shared file");
        fs::hard_link(root.join("a/meta.yml"), root.join("a/alias.yml")).expect("hard link");
        let config = Config::from_yaml(
            "contract: contract-1\nrelease-units:\n  alpha:\n    path: a\n    projections:\n      - { adapter: yaml, file: meta.yml, pointer: /version, mode: committed }\n      - { adapter: yaml, file: alias.yml, pointer: version, mode: injected }\n    tags:\n      primary: { role: primary, template: '{version}' }\n",
        )
        .expect("lexically distinct files load");
        let error = check_physical_ownership(root, &config)
            .expect_err("one physical field refused")
            .to_string();
        let expected = format!(
            "release unit alpha projects /version twice in one file: {} and {} are the same file",
            Path::new("a").join("meta.yml").display(),
            Path::new("a").join("alias.yml").display()
        );
        assert!(error.contains(&expected), "{error}");

        let ignored: Config = Config::from_yaml(
            &config
                .to_yaml()
                .expect("config yaml")
                .replace("mode: injected", "mode: none"),
        )
        .expect("mode none loads");
        check_physical_ownership(root, &ignored).expect("mode none does not own a field");

        fs::remove_file(root.join("a/alias.yml")).expect("remove link");
        fs::write(root.join("a/alias.yml"), "version: 1.0.0\n").expect("distinct file");
        check_physical_ownership(root, &config).expect("distinct files accepted");
    }
}
