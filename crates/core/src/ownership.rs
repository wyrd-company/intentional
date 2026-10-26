// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Ownership of the version fields that projections write.

use crate::adapters::format::{json_pointer, key_pointer};
use crate::config::{Projection, ReleaseUnitConfig};
use crate::error::{Error, Result};
use crate::model::{Adapter, ProjectionMode};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

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
