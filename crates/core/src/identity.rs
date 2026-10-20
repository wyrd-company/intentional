// ---
// relationships:
//   implements: release-model
// ---

//! Native package identity of a manifest, shared by initialization and the
//! Release Model.

use crate::error::{Error, Result};
use crate::model::Adapter;
use serde_json::Value as JsonValue;
use std::path::Path;

/// Resolve the native package identity that `adapter` declares in `text`.
///
/// `path` is the manifest location. It names the manifest in parse errors and
/// gives the MSBuild project file stem. The rule per adapter is: npm `name`,
/// Cargo `package.name`, Pub `name`, PEP 621 `project.name`, Go `module`
/// path, and MSBuild `PackageId`, else `AssemblyName`, else the project file
/// name without its extension.
///
/// Returns `Ok(None)` when the rule finds no identity in a readable manifest
/// and for the generic JSON, TOML and YAML adapters. Returns an error only
/// when the manifest cannot be parsed for its adapter.
pub fn native_identity(adapter: Adapter, path: &Path, text: &str) -> Result<Option<String>> {
    Ok(match adapter {
        Adapter::Npm => {
            let value: JsonValue = serde_json::from_str(text).map_err(|error| {
                Error::Validation(format!("invalid {}: {error}", path.display()))
            })?;
            value["name"].as_str().map(str::to_owned)
        }
        Adapter::Cargo => toml_table_string(path, text, "package")?,
        Adapter::Pub => {
            let value: serde_yaml::Value = serde_yaml::from_str(text)?;
            value["name"].as_str().map(str::to_owned)
        }
        Adapter::Python => toml_table_string(path, text, "project")?,
        Adapter::Go => text
            .lines()
            .find_map(|line| line.trim().strip_prefix("module "))
            .map(str::to_owned),
        Adapter::Msbuild => xml_element(text, "PackageId")
            .or_else(|| xml_element(text, "AssemblyName"))
            .or_else(|| {
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .map(str::to_owned)
            }),
        Adapter::Json | Adapter::Toml | Adapter::Yaml => None,
    })
}

fn toml_table_string(path: &Path, text: &str, table: &str) -> Result<Option<String>> {
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| Error::Validation(format!("invalid {}: {error}", path.display())))?;
    Ok(document
        .get(table)
        .and_then(|item| item.get("name"))
        .and_then(toml_edit::Item::as_str)
        .map(str::to_owned))
}

/// Trimmed contents of the first `<name>` element.
pub(crate) fn xml_element(text: &str, name: &str) -> Option<String> {
    let start = format!("<{name}>");
    let end = format!("</{name}>");
    text.split_once(&start)?
        .1
        .split_once(&end)
        .map(|(value, _)| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(adapter: Adapter, file: &str, text: &str) -> Option<String> {
        native_identity(adapter, Path::new(file), text).expect("readable manifest")
    }

    #[test]
    fn each_ecosystem_adapter_reads_its_identity_field() {
        let cases = [
            (
                Adapter::Npm,
                "package.json",
                r#"{"name":"sample-library","version":"1.0.0"}"#,
            ),
            (
                Adapter::Cargo,
                "Cargo.toml",
                "[package]\nname = \"sample-library\"\nversion = \"1.0.0\"\n",
            ),
            (
                Adapter::Pub,
                "pubspec.yaml",
                "name: sample-library\nversion: 1.0.0\n",
            ),
            (
                Adapter::Python,
                "pyproject.toml",
                "[project]\nname = \"sample-library\"\nversion = \"1.0.0\"\n",
            ),
            (
                Adapter::Go,
                "go.mod",
                "// comment\nmodule sample-library\n\ngo 1.22\n",
            ),
        ];
        for (adapter, file, text) in cases {
            assert_eq!(
                identity(adapter, file, text).as_deref(),
                Some("sample-library"),
                "{adapter:?}"
            );
        }
    }

    #[test]
    fn msbuild_prefers_package_id() {
        let text = "<Project><PropertyGroup><AssemblyName>sample-assembly</AssemblyName><PackageId>sample-package</PackageId></PropertyGroup></Project>";
        assert_eq!(
            identity(Adapter::Msbuild, "src/Sample.csproj", text).as_deref(),
            Some("sample-package")
        );
    }

    #[test]
    fn msbuild_falls_back_to_assembly_name() {
        let text = "<Project><PropertyGroup><AssemblyName>sample-assembly</AssemblyName></PropertyGroup></Project>";
        assert_eq!(
            identity(Adapter::Msbuild, "src/Sample.csproj", text).as_deref(),
            Some("sample-assembly")
        );
    }

    #[test]
    fn msbuild_falls_back_to_project_file_stem() {
        let text = "<Project><PropertyGroup><Version>1.0.0</Version></PropertyGroup></Project>";
        assert_eq!(
            identity(Adapter::Msbuild, "src/Sample.Project.csproj", text).as_deref(),
            Some("Sample.Project")
        );
    }

    #[test]
    fn readable_manifest_without_identity_gives_none() {
        let cases = [
            (Adapter::Npm, "package.json", r#"{"version":"1.0.0"}"#),
            (Adapter::Npm, "package.json", r#"{"name":7}"#),
            (
                Adapter::Cargo,
                "Cargo.toml",
                "[package]\nversion = \"1.0.0\"\n",
            ),
            (Adapter::Cargo, "Cargo.toml", "[workspace]\nmembers = []\n"),
            (Adapter::Pub, "pubspec.yaml", "version: 1.0.0\n"),
            (
                Adapter::Python,
                "pyproject.toml",
                "[project]\nversion = \"1.0.0\"\n",
            ),
            (
                Adapter::Python,
                "pyproject.toml",
                "[tool.sample]\nkey = 1\n",
            ),
            (Adapter::Go, "go.mod", "go 1.22\n"),
        ];
        for (adapter, file, text) in cases {
            assert_eq!(identity(adapter, file, text), None, "{adapter:?} {text}");
        }
    }

    #[test]
    fn generic_adapters_carry_no_identity() {
        for (adapter, file, text) in [
            (Adapter::Json, "sample.json", r#"{"name":"sample-library"}"#),
            (Adapter::Toml, "sample.toml", "name = \"sample-library\"\n"),
            (Adapter::Yaml, "sample.yaml", "name: sample-library\n"),
        ] {
            assert_eq!(identity(adapter, file, text), None, "{adapter:?}");
        }
    }

    #[test]
    fn unreadable_manifest_is_an_error() {
        for (adapter, file, text) in [
            (Adapter::Npm, "package.json", "{"),
            (Adapter::Cargo, "Cargo.toml", "[package"),
            (Adapter::Pub, "pubspec.yaml", "name: [unclosed"),
            (Adapter::Python, "pyproject.toml", "[project"),
        ] {
            assert!(
                native_identity(adapter, Path::new(file), text).is_err(),
                "{adapter:?}"
            );
        }
    }
}
