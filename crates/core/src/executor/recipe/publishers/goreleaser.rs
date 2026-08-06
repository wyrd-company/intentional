// ---
// relationships:
//   implements: github-release-executor
// ---

// GoReleaser-backed publication recipe selection moved from `executor::recipe`
// so publisher routes can change independently.

/// Arch User Repository package one release unit publishes, from native evidence.
///
/// The destination is whatever the packager wrote and registered, which is not
/// what the repository declared: the packager resolves an unnamed entry to the
/// project name and then suffixes every name with `-bin` unless it already ends
/// that way. Reading the declaration verbatim would name a package that does not
/// exist, so the same rule the packager applies is applied here.
fn aur_package(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
    identity: &str,
) -> Result<Option<String>> {
    let directory = root.join(&release_unit.path);
    let config = crate::executor::goreleaser::read(&directory)?;
    let Some(project) =
        crate::executor::goreleaser::subject_identity_from(&directory, config.as_ref())?
    else {
        return Ok(None);
    };
    let path = release_unit.path.join(
        config
            .as_ref()
            .map(|config| config.path.as_path())
            .unwrap_or(Path::new(".goreleaser.yaml")),
    );
    let declared_origin = format!("{} aur[0].name", path.display());
    let project_origin = if config
        .as_ref()
        .is_some_and(|config| config.project_name.is_some())
    {
        format!("{} project_name", path.display())
    } else {
        format!(
            "{} module basename",
            release_unit.path.join("go.mod").display()
        )
    };
    let declared = config
        .as_ref()
        .and_then(|config| config.aur_names.first())
        .and_then(Option::as_deref)
        .map(|value| crate::executor::names::SuppliedName {
            origin: &declared_origin,
            value,
        });
    if declared
        .as_ref()
        .is_some_and(|supplied| crate::executor::goreleaser::is_templated_name(supplied.value))
    {
        return Err(Error::Validation(format!(
            "{identity} cannot publish templated aur[0].name in {}; maintained Arch publication requires a literal package name",
            path.display()
        )));
    }
    let project = crate::executor::names::SuppliedName {
        origin: &project_origin,
        value: &project,
    };
    crate::executor::goreleaser::arch_package_name(declared.as_ref(), &project)
        .map(Some)
        .map_err(Error::Validation)
}


/// Every discoverable main-package directory in one Go module.
pub(crate) fn go_main_package_directories(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for entry in WalkDir::new(directory)
        .into_iter()
        .filter_entry(go_package_walk_entry)
    {
        let entry = entry.map_err(|error| {
            Error::Validation(format!(
                "cannot inspect Go module {}: {error}",
                directory.display()
            ))
        })?;
        if entry.file_type().is_dir() && declares_main_package(entry.path())? {
            roots.push(entry.into_path());
        }
    }
    Ok(roots)
}

/// Whether Go's recursive package pattern includes a tree entry in this module.
///
/// Go excludes vendored and test-data trees plus directories whose names begin
/// with `.` or `_`. A nested `go.mod` starts another module, so its packages are
/// discovered when that manifest is processed instead of being attributed to
/// its parent module.
fn go_package_walk_entry(entry: &DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !matches!(name.as_ref(), "vendor" | "testdata")
        && !name.starts_with(['.', '_'])
        && !entry.path().join("go.mod").is_file()
}

/// Whether the Go files directly inside one directory declare `package main`.
///
/// The clause is read as Go declares it rather than as an exact line, because a
/// build-constrained file and a file carrying an import comment both spell the
/// package clause with something after it and both are ordinary Go.
fn declares_main_package(directory: &Path) -> Result<bool> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(false);
    };
    for entry in entries {
        let path = entry.map_err(|error| Error::io(directory, error))?.path();
        if path.extension().is_none_or(|extension| extension != "go") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.lines().any(is_main_package_clause) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether one source line is the `package main` clause.
fn is_main_package_clause(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("package ") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix("main") else {
        return false;
    };
    let rest = rest.trim_start_matches(';').trim();
    rest.is_empty() || rest.starts_with("//") || rest.starts_with("/*")
}
