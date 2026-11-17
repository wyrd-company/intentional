// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Ownership of the version fields that projections write.

use crate::adapters::format::{json_pointer, key_pointer};
use crate::adapters::CargoAdapter;
use crate::apply::workspace_manifest;
use crate::config::{Config, Projection, ReleaseUnitConfig};
use crate::error::{Error, Result};
use crate::model::{Adapter, ProjectionMode};
use crate::workspace_files::{contained, read_contained, FileIdentity};
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

/// Whether a projection writes its field.
///
/// Mode `none` neither reads nor writes, except for Go: apply rewrites the
/// module path of every Go projection on a major release.
fn writes(projection: &Projection) -> bool {
    projection.mode != ProjectionMode::None || projection.adapter == Adapter::Go
}

/// Refuse two projections that write one field of one file.
///
/// Files compare by their resolved workspace path, so `/docs/x.yml` on any
/// unit and `docs/x.yml` on a unit at `.` are one file. Projections that do
/// not write their field do not count.
pub(crate) fn check_ownership(release_units: &BTreeMap<String, ReleaseUnitConfig>) -> Result<()> {
    let mut owners = BTreeMap::<(PathBuf, ProjectionField), &str>::new();
    for (id, release_unit) in release_units {
        for projection in &release_unit.projections {
            if !writes(projection) {
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

/// The physical field that one projection writes.
struct PhysicalTarget {
    identity: FileIdentity,
    field: ProjectionField,
    /// The file as reported in a refusal.
    display: String,
    /// A Cargo member that writes its workspace's inherited version.
    inherited: bool,
}

impl PhysicalTarget {
    /// Resolve the file and field a projection writes.
    ///
    /// A Cargo member whose version is inherited writes
    /// `/workspace/package/version` in its workspace manifest, not its own
    /// manifest. A member that cannot be read keeps its own field; the
    /// command that reads it then fails.
    fn of(root: &Path, path: PathBuf, projection: &Projection) -> Result<Self> {
        if projection.adapter == Adapter::Cargo {
            let inherited = read_contained(root, &path)
                .ok()
                .and_then(|text| CargoAdapter.version(&text).ok())
                .is_some_and(|version| version.is_none());
            if inherited {
                let workspace = workspace_manifest(root, &path)?;
                return Ok(Self {
                    identity: FileIdentity::of(&contained(root, &workspace)?)?,
                    field: ProjectionField::Keys(
                        ["workspace", "package", "version"]
                            .map(str::to_owned)
                            .to_vec(),
                    ),
                    display: format!("{} (inherited by {})", workspace.display(), path.display()),
                    inherited: true,
                });
            }
        }
        Ok(Self {
            identity: FileIdentity::of(&contained(root, &path)?)?,
            field: ProjectionField::of(projection),
            display: path.display().to_string(),
            inherited: false,
        })
    }

    /// Whether two projections write one field. Cargo members that inherit
    /// one workspace version share it; apply refuses them when their
    /// versions differ.
    fn conflicts_with(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.field == other.field
            && !(self.inherited && other.inherited)
    }
}

/// Refuse two projections that write one field of one physical file.
///
/// Config load compares paths as text, so it cannot see two paths that
/// reach one file through symlinks, or through letter case on a
/// filesystem that ignores it, or a Cargo member that writes its
/// workspace's inherited version. Commands that edit or read projections
/// run this check against the filesystem.
pub(crate) fn check_physical_ownership(root: &Path, config: &Config) -> Result<()> {
    let mut owners = Vec::<(PhysicalTarget, &str)>::new();
    for (id, release_unit) in &config.release_units {
        for projection in &release_unit.projections {
            if !writes(projection) {
                continue;
            }
            let path = projection.file.workspace_path(&release_unit.path);
            let target = PhysicalTarget::of(root, path, projection)?;
            if let Some((other, owner)) = owners
                .iter()
                .find(|(known, _)| known.conflicts_with(&target))
            {
                let field = &target.field;
                let owners = if *owner == id.as_str() {
                    format!("release unit {id} projects {field} twice")
                } else {
                    format!("release units {owner} and {id} both project {field}")
                };
                return Err(Error::Validation(format!(
                    "{owners} in one file: {} and {} are the same file",
                    other.display, target.display
                )));
            }
            owners.push((target, id));
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
            "contract: contract-2\nrelease-units:\n  alpha:\n    path: a\n    projections:\n      - { adapter: yaml, file: meta.yml, pointer: /version, mode: committed }\n      - { adapter: yaml, file: alias.yml, pointer: version, mode: injected }\n    tags:\n      primary: { role: primary, template: '{version}' }\n",
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

        let go = Config::from_yaml(
            "contract: contract-2\nrelease-units:\n  alpha:\n    path: a\n    projections:\n      - { adapter: go, file: meta.yml, mode: none }\n      - { adapter: go, file: alias.yml, mode: none }\n    tags:\n      primary: { role: primary, template: '{version}' }\n",
        )
        .expect("lexically distinct module files load");
        let error = check_physical_ownership(root, &go)
            .expect_err("one module path refused in mode none")
            .to_string();
        assert!(
            error.contains("release unit alpha projects the module path twice"),
            "{error}"
        );

        fs::remove_file(root.join("a/alias.yml")).expect("remove link");
        fs::write(root.join("a/alias.yml"), "version: 1.0.0\n").expect("distinct file");
        check_physical_ownership(root, &config).expect("distinct files accepted");
    }
}
