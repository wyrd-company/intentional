// ---
// relationships:
//   implements: github-release-executor
// ---

//! Native GoReleaser configuration the maintained Go recipes read.
//!
//! GoReleaser configuration is native project state rather than duplicated
//! executor configuration, so everything the derivation needs about a Go release
//! unit — where its command lives, what its published artifacts are named, and
//! which distribution pipes it declares — is read from the repository's own
//! `.goreleaser.yaml` and `go.mod`.
//!
//! Reading is deliberately shallow. This is not a GoReleaser parser: it reads
//! the few members the derivation and the native-contract check depend on, and
//! it never rewrites them. A member this does not read is one GoReleaser owns.

use crate::error::{Error, Result};
use crate::executor::recipe::Packager;
use crate::model::PublisherKind;
use std::path::{Path, PathBuf};

/// Distribution pipe one publisher adapter's deliverables come out of.
///
/// Each maintained recipe promotes what one GoReleaser pipe produced, so a
/// configured publisher with no corresponding pipe has nothing to promote. The
/// mapping is fixed by GoReleaser rather than by configuration: `brews` writes
/// the Homebrew formula, `nfpms` writes the system packages RPM and APT consume,
/// and `aur` writes the Arch package sources.
pub const fn pipe(publisher: PublisherKind) -> Option<&'static str> {
    match publisher {
        PublisherKind::Homebrew => Some("brews"),
        PublisherKind::Rpm | PublisherKind::Apt => Some("nfpms"),
        PublisherKind::Aur => Some("aur"),
        PublisherKind::Npm | PublisherKind::Cargo | PublisherKind::Oci => None,
    }
}

/// Native package format one system-package publisher's deliverable carries.
///
/// `nfpms` builds several formats from one declaration, so RPM and APT are one
/// pipe and two formats. A release unit that declares `nfpms` without the format
/// a configured publisher distributes produces no deliverable for it.
pub const fn nfpm_format(publisher: PublisherKind) -> Option<&'static str> {
    match publisher {
        PublisherKind::Rpm => Some("rpm"),
        PublisherKind::Apt => Some("deb"),
        _ => None,
    }
}

/// One release unit's native GoReleaser configuration, as far as it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoReleaserConfig {
    /// Release-unit-relative file the configuration was read from.
    pub path: PathBuf,
    /// Declared `project_name`, when the configuration states one.
    pub project_name: Option<String>,
    /// Main package directories `builds[].main` names, release-unit relative.
    pub main_directories: Vec<PathBuf>,
    /// Distribution pipes the configuration declares.
    pub pipes: Vec<String>,
    /// Formats `nfpms` declares across every entry.
    pub nfpm_formats: Vec<String>,
    /// Arch package names `aur` declares, in declaration order.
    pub aur_names: Vec<String>,
}

/// Read one release unit's native GoReleaser configuration.
///
/// Absence is not a failure here. A release unit can derive the Go application
/// capability from its sources alone, and whether the packager's configuration
/// has to exist is the native-contract check's question rather than the
/// reader's.
pub fn read(directory: &Path) -> Result<Option<GoReleaserConfig>> {
    for name in Packager::GoReleaser.configuration_paths() {
        let path = directory.join(name);
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
        let document = serde_yaml::from_str::<serde_yaml::Value>(&text)
            .map_err(|error| Error::Validation(format!("{name} is not valid YAML: {error}")))?;
        return Ok(Some(parse(Path::new(name), &document)));
    }
    Ok(None)
}

/// Project the members the derivation reads out of one parsed document.
fn parse(name: &Path, document: &serde_yaml::Value) -> GoReleaserConfig {
    let mapping = document.as_mapping();
    let pipes = mapping
        .map(|mapping| {
            mapping
                .keys()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    GoReleaserConfig {
        path: name.to_owned(),
        project_name: document
            .get("project_name")
            .and_then(serde_yaml::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        main_directories: main_directories(document),
        pipes,
        nfpm_formats: sequence(document, "nfpms")
            .iter()
            .flat_map(|entry| {
                entry
                    .get("formats")
                    .and_then(serde_yaml::Value::as_sequence)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(serde_yaml::Value::as_str)
                    .map(str::to_owned)
            })
            .collect(),
        aur_names: sequence(document, "aur")
            .iter()
            .filter_map(|entry| entry.get("name").and_then(serde_yaml::Value::as_str))
            .map(str::to_owned)
            .collect(),
    }
}

/// Entries of one top-level sequence member.
fn sequence<'a>(document: &'a serde_yaml::Value, member: &str) -> &'a [serde_yaml::Value] {
    document
        .get(member)
        .and_then(serde_yaml::Value::as_sequence)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Main package directories `builds[].main` names, relative to the release unit.
///
/// GoReleaser resolves `main` against its own working directory, which the
/// derived build job sets to the release unit, so the same relative resolution
/// applies. `main` names either a package directory or one file inside it, and
/// both forms mean the same package. A path escaping the release unit is
/// discarded rather than followed: derivation reads the unit it is deriving.
fn main_directories(document: &serde_yaml::Value) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for build in sequence(document, "builds") {
        let Some(main) = build.get("main").and_then(serde_yaml::Value::as_str) else {
            continue;
        };
        let relative = Path::new(main.trim().trim_start_matches("./"));
        let relative = if relative
            .extension()
            .is_some_and(|extension| extension == "go")
        {
            relative.parent().unwrap_or(Path::new("")).to_owned()
        } else {
            relative.to_owned()
        };
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            continue;
        }
        directories.push(relative);
    }
    directories
}

/// Module path one release unit's `go.mod` declares.
pub fn module_path(directory: &Path) -> Result<Option<String>> {
    let path = directory.join("go.mod");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
    Ok(text.lines().find_map(|line| {
        line.strip_prefix("module ")
            .map(|module| module.trim().trim_matches('"').to_owned())
            .filter(|module| !module.is_empty())
    }))
}

/// Identity every GoReleaser destination resolves one subject by.
///
/// GoReleaser names the Homebrew formula, the system packages, and the Arch
/// package from one project name, so that name is the subject identity every
/// destination of a Go release unit resolves. Where the configuration declares
/// it the derivation reads it; where it does not, the module path's last element
/// is what Go itself names the command, and it is what GoReleaser's own default
/// resolves to for a repository whose module is its project.
///
/// The native-contract check requires the explicit declaration, so the module
/// fallback is a total function's last arm rather than a value a real
/// publication depends on.
pub fn subject_identity(directory: &Path) -> Result<Option<String>> {
    if let Some(config) = read(directory)? {
        if let Some(name) = config.project_name {
            return Ok(Some(name));
        }
    }
    Ok(module_path(directory)?.and_then(|module| {
        module
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;

    #[test]
    fn reads_the_members_the_derivation_depends_on() {
        let workspace = Workspace::new("goreleaser-read");
        workspace.write(
            "component/.goreleaser.yaml",
            "version: 2\nproject_name: example-tool\nbuilds:\n  - main: ./cmd/example\n  - main: ./tools/other/main.go\nbrews:\n  - repository: { owner: example-org, name: homebrew-tap }\nnfpms:\n  - formats: [ rpm, deb ]\n",
        );
        let config = read(&workspace.root().join("component"))
            .expect("configuration reads")
            .expect("configuration is present");
        assert_eq!(config.project_name.as_deref(), Some("example-tool"));
        assert_eq!(
            config.main_directories,
            vec![PathBuf::from("cmd/example"), PathBuf::from("tools/other")]
        );
        assert!(config.pipes.contains(&"brews".to_owned()));
        assert_eq!(
            config.nfpm_formats,
            vec!["rpm".to_owned(), "deb".to_owned()]
        );
    }

    #[test]
    fn discards_a_main_path_that_escapes_the_release_unit() {
        let workspace = Workspace::new("goreleaser-escape");
        workspace.write(
            "component/.goreleaser.yaml",
            "version: 2\nbuilds:\n  - main: ../other/cmd\n  - main: /etc/cmd\n",
        );
        let config = read(&workspace.root().join("component"))
            .expect("configuration reads")
            .expect("configuration is present");
        assert!(
            config.main_directories.is_empty(),
            "derivation reads the release unit it is deriving"
        );
    }

    #[test]
    fn derives_the_subject_identity_from_native_evidence() {
        let workspace = Workspace::new("goreleaser-identity");
        let directory = workspace.root().join("component");
        workspace.write("component/go.mod", "module example.test/example-tool\n");
        assert_eq!(
            subject_identity(&directory).expect("identity derives"),
            Some("example-tool".to_owned()),
            "the module path's last element is what Go names the command"
        );

        workspace.write(
            "component/.goreleaser.yaml",
            "version: 2\nproject_name: declared-tool\n",
        );
        assert_eq!(
            subject_identity(&directory).expect("identity derives"),
            Some("declared-tool".to_owned()),
            "an explicit project name is the authority"
        );
    }

    #[test]
    fn reports_a_configuration_that_is_not_valid_yaml() {
        let workspace = Workspace::new("goreleaser-malformed");
        workspace.write("component/.goreleaser.yaml", "builds:\n  - main: [\n");
        let error = read(&workspace.root().join("component"))
            .expect_err("a malformed document is reported");
        assert!(
            error
                .to_string()
                .contains(".goreleaser.yaml is not valid YAML"),
            "{error}"
        );
    }
}
