// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Thin npm and Cargo specializations over JSON and TOML format adapters.

use super::format::{FormatAdapter, JsonFormat, TomlFormat, YamlFormat};
use crate::error::{Error, Result};
use semver::Version as SemverVersion;
use toml_edit::{value, DocumentMut, Item, Value};

/// npm `package.json` adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct NpmAdapter;

impl NpmAdapter {
    /// Read the npm package name.
    pub fn name(self, text: &str) -> Result<String> {
        JsonFormat.read_text(text, "/name")
    }

    /// Read the manifest version.
    pub fn version(self, text: &str) -> Result<String> {
        JsonFormat.read_text(text, "/version")
    }

    /// Replace only the npm version value.
    pub fn edit_version(self, text: &str, version: &str) -> Result<String> {
        JsonFormat.edit_text(text, "/version", version)
    }

    /// Rewrite an internal dependency range wherever npm permits it.
    pub fn edit_dependency(self, text: &str, name: &str, version: &str) -> Result<String> {
        let mut edited = text.to_owned();
        let mut found = false;
        for group in ["dependencies", "devDependencies", "peerDependencies"] {
            let pointer = format!("/{group}/{}", escape_json_pointer(name));
            if let Ok(existing) = JsonFormat.read_text(&edited, &pointer) {
                let range = npm_range(&existing, version);
                edited = JsonFormat.edit_text(&edited, &pointer, &range)?;
                found = true;
            }
        }
        if !found {
            return Err(Error::Validation(format!(
                "npm manifest does not declare internal dependency {name}"
            )));
        }
        Ok(edited)
    }
}

/// Cargo `Cargo.toml` adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct CargoAdapter;

/// Dart/Flutter `pubspec.yaml` adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct PubAdapter;

/// PEP 621 `pyproject.toml` adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct PythonAdapter;

/// MSBuild project adapter using targeted XML element edits.
#[derive(Debug, Clone, Copy, Default)]
pub struct MsbuildAdapter;

/// Go module adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoAdapter;

impl CargoAdapter {
    /// Read the Cargo package name.
    pub fn name(self, text: &str) -> Result<String> {
        TomlFormat.read_text(text, "/package/name")
    }

    /// Read the package version, or `None` when inherited from the workspace.
    pub fn version(self, text: &str) -> Result<Option<String>> {
        match TomlFormat.read_text(text, "/package/version") {
            Ok(version) => Ok(Some(version)),
            Err(_) if cargo_version_is_inherited(text)? => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Replace the package version. Inherited versions must be written at the workspace root.
    pub fn edit_version(self, text: &str, version: &str) -> Result<String> {
        if cargo_version_is_inherited(text)? {
            return Err(Error::Validation(
                "Cargo package inherits workspace version".to_owned(),
            ));
        }
        TomlFormat.edit_text(text, "/package/version", version)
    }

    /// Replace `[workspace.package].version` in a workspace root manifest.
    pub fn edit_workspace_version(self, text: &str, version: &str) -> Result<String> {
        TomlFormat.edit_text(text, "/workspace/package/version", version)
    }

    /// Rewrite a Cargo dependency version in package or workspace dependencies.
    pub fn edit_dependency(self, text: &str, name: &str, version: &str) -> Result<String> {
        let mut document = text
            .parse::<DocumentMut>()
            .map_err(|error| Error::Validation(format!("invalid TOML: {error}")))?;
        let found = rewrite_dependency_table(document.get_mut("dependencies"), name, version)?
            | rewrite_dependency_table(
                document
                    .get_mut("workspace")
                    .and_then(|workspace| workspace.get_mut("dependencies")),
                name,
                version,
            )?;
        if !found {
            return Err(Error::Validation(format!(
                "Cargo manifest does not declare internal dependency {name}"
            )));
        }
        Ok(document.to_string())
    }

    /// Whether a dependency is inherited through `{ workspace = true }`.
    pub fn dependency_is_inherited(self, text: &str, name: &str) -> Result<bool> {
        let document = text
            .parse::<DocumentMut>()
            .map_err(|error| Error::Validation(format!("invalid TOML: {error}")))?;
        let dependency = document
            .get("dependencies")
            .and_then(|dependencies| dependencies.get(name));
        Ok(dependency.is_some_and(item_workspace_true))
    }
}

impl PubAdapter {
    /// Read the Pub package name.
    pub fn name(self, text: &str) -> Result<String> {
        YamlFormat.read_text(text, "/name")
    }

    /// Read the Pub package version.
    pub fn version(self, text: &str) -> Result<String> {
        YamlFormat.read_text(text, "/version")
    }

    /// Replace the Pub package version.
    pub fn edit_version(self, text: &str, version: &str) -> Result<String> {
        YamlFormat.edit_text(text, "/version", version)
    }

    /// Rewrite a direct Pub dependency constraint.
    pub fn edit_dependency(self, text: &str, name: &str, version: &str) -> Result<String> {
        YamlFormat.edit_text(
            text,
            &format!("/dependencies/{name}"),
            &format!("^{version}"),
        )
    }
}

impl PythonAdapter {
    /// Read the PEP 621 project name.
    pub fn name(self, text: &str) -> Result<String> {
        TomlFormat.read_text(text, "/project/name")
    }

    /// Read the PEP 621 project version.
    pub fn version(self, text: &str) -> Result<String> {
        TomlFormat.read_text(text, "/project/version")
    }

    /// Replace the PEP 621 project version.
    pub fn edit_version(self, text: &str, version: &str) -> Result<String> {
        TomlFormat.edit_text(text, "/project/version", version)
    }

    /// Rewrite a PEP 508 dependency string in `project.dependencies`.
    pub fn edit_dependency(self, text: &str, name: &str, version: &str) -> Result<String> {
        let mut document = text
            .parse::<DocumentMut>()
            .map_err(|error| Error::Validation(format!("invalid TOML: {error}")))?;
        let array = document
            .get_mut("project")
            .and_then(|project| project.get_mut("dependencies"))
            .and_then(Item::as_array_mut)
            .ok_or_else(|| {
                Error::Validation("Python project.dependencies must be an array".to_owned())
            })?;
        let mut found = false;
        for dependency in array.iter_mut() {
            let Some(existing) = dependency.as_str() else {
                continue;
            };
            if python_dependency_name(existing) != name {
                continue;
            }
            let decor = dependency.decor().clone();
            *dependency = Value::from(format!("{name}>={version}"));
            *dependency.decor_mut() = decor;
            found = true;
        }
        if !found {
            return Err(Error::Validation(format!(
                "Python manifest does not declare internal dependency {name}"
            )));
        }
        Ok(document.to_string())
    }
}

impl MsbuildAdapter {
    /// Read `<PackageId>` when present.
    pub fn name(self, text: &str) -> Result<String> {
        read_xml_element(text, "PackageId")
    }

    /// Read the `<Version>` element.
    pub fn version(self, text: &str) -> Result<String> {
        read_xml_element(text, "Version")
    }

    /// Replace only the contents of the first `<Version>` element.
    pub fn edit_version(self, text: &str, version: &str) -> Result<String> {
        edit_xml_element(text, "Version", version)
    }

    /// Rewrite a `PackageReference` version attribute or nested element.
    pub fn edit_dependency(self, text: &str, name: &str, version: &str) -> Result<String> {
        let needle = format!("Include=\"{name}\"");
        let include = text.find(&needle).ok_or_else(|| {
            Error::Validation(format!(
                "MSBuild project does not declare PackageReference {name}"
            ))
        })?;
        let tag_start = text[..include]
            .rfind('<')
            .ok_or_else(|| Error::Validation("invalid MSBuild PackageReference".to_owned()))?;
        let tag_end = text[include..]
            .find('>')
            .map(|offset| include + offset)
            .ok_or_else(|| Error::Validation("unterminated PackageReference".to_owned()))?;
        let opening = &text[tag_start..=tag_end];
        if let Some(version_attribute) = opening.find("Version=\"") {
            let start = tag_start + version_attribute + "Version=\"".len();
            let end = text[start..]
                .find('"')
                .map(|offset| start + offset)
                .ok_or_else(|| Error::Validation("unterminated Version attribute".to_owned()))?;
            let mut edited = text.to_owned();
            edited.replace_range(start..end, version);
            return Ok(edited);
        }
        let close = text[tag_end + 1..]
            .find("</PackageReference>")
            .map(|offset| tag_end + 1 + offset)
            .ok_or_else(|| Error::Validation(format!("PackageReference {name} has no version")))?;
        let body = &text[tag_end + 1..close];
        let body_edited = edit_xml_element(body, "Version", version)?;
        let mut edited = text.to_owned();
        edited.replace_range(tag_end + 1..close, &body_edited);
        Ok(edited)
    }
}

impl GoAdapter {
    /// Read the module path.
    pub fn name(self, text: &str) -> Result<String> {
        go_module_line(text)
            .map(|(_, value)| value.to_owned())
            .ok_or_else(|| Error::Validation("go.mod has no module directive".to_owned()))
    }

    /// Rewrite the `/vN` suffix for a major release at version 2 or later.
    pub fn edit_major_module_path(self, text: &str, version: &str) -> Result<String> {
        let version = SemverVersion::parse(version)?;
        if version.major < 2 {
            return Ok(text.to_owned());
        }
        let (range, module) = go_module_line(text)
            .ok_or_else(|| Error::Validation("go.mod has no module directive".to_owned()))?;
        let base = module
            .rsplit_once("/v")
            .filter(|(_, suffix)| suffix.chars().all(|character| character.is_ascii_digit()))
            .map(|(base, _)| base)
            .unwrap_or(module);
        let mut edited = text.to_owned();
        edited.replace_range(range, &format!("{base}/v{}", version.major));
        Ok(edited)
    }

    /// Rewrite a direct `require` version while preserving the mandatory Go `v` syntax.
    pub fn edit_dependency(self, text: &str, module: &str, version: &str) -> Result<String> {
        let mut offset = 0;
        for line_with_newline in text.split_inclusive('\n') {
            let line = line_with_newline
                .strip_suffix('\n')
                .unwrap_or(line_with_newline);
            let trimmed = line.trim();
            let fields = trimmed.split_whitespace().collect::<Vec<_>>();
            let matches = (fields.len() >= 3 && fields[0] == "require" && fields[1] == module)
                || (fields.len() >= 2 && fields[0] == module);
            if matches {
                let old = fields.last().expect("matched fields are non-empty");
                let start = offset + line.find(old).expect("field comes from line");
                let mut edited = text.to_owned();
                edited.replace_range(start..start + old.len(), &format!("v{version}"));
                return Ok(edited);
            }
            offset += line_with_newline.len();
        }
        Err(Error::Validation(format!(
            "go.mod does not require internal module {module}"
        )))
    }
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn npm_range(existing: &str, version: &str) -> String {
    let workspace = existing.starts_with("workspace:");
    let without_workspace = existing.strip_prefix("workspace:").unwrap_or(existing);
    let operator = if without_workspace.starts_with('~') {
        "~"
    } else if without_workspace.starts_with("^") {
        "^"
    } else if without_workspace.starts_with(">=") {
        ">="
    } else {
        ""
    };
    format!(
        "{}{}{}",
        if workspace { "workspace:" } else { "" },
        operator,
        version
    )
}

fn cargo_version_is_inherited(text: &str) -> Result<bool> {
    let document = text
        .parse::<DocumentMut>()
        .map_err(|error| Error::Validation(format!("invalid TOML: {error}")))?;
    Ok(document
        .get("package")
        .and_then(|package| package.get("version"))
        .is_some_and(item_workspace_true))
}

fn item_workspace_true(item: &Item) -> bool {
    item.as_inline_table()
        .and_then(|table| table.get("workspace"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || item
            .as_table()
            .and_then(|table| table.get("workspace"))
            .and_then(Item::as_bool)
            .unwrap_or(false)
}

fn rewrite_dependency_table(table: Option<&mut Item>, name: &str, version: &str) -> Result<bool> {
    let Some(table) = table.and_then(Item::as_table_like_mut) else {
        return Ok(false);
    };
    let Some(dependency) = table.get_mut(name) else {
        return Ok(false);
    };
    if item_workspace_true(dependency) {
        return Ok(false);
    }
    match dependency {
        Item::Value(Value::String(old)) => {
            let decor = old.decor().clone();
            *dependency = value(version);
            if let Some(new) = dependency.as_value_mut() {
                *new.decor_mut() = decor;
            }
        }
        Item::Value(Value::InlineTable(table)) => {
            let old = table.get("version").ok_or_else(|| {
                Error::Validation(format!("Cargo dependency {name} has no version"))
            })?;
            let decor = old.decor().clone();
            let mut new = Value::from(version);
            *new.decor_mut() = decor;
            table.insert("version", new);
        }
        Item::Table(table) => {
            let old = table.get("version").ok_or_else(|| {
                Error::Validation(format!("Cargo dependency {name} has no version"))
            })?;
            let decor = old
                .as_value()
                .map(|value| value.decor().clone())
                .unwrap_or_default();
            table.insert("version", value(version));
            if let Some(new) = table.get_mut("version").and_then(Item::as_value_mut) {
                *new.decor_mut() = decor;
            }
        }
        _ => {
            return Err(Error::Validation(format!(
                "unsupported Cargo dependency declaration for {name}"
            )));
        }
    }
    Ok(true)
}

fn python_dependency_name(value: &str) -> &str {
    let end = value
        .char_indices()
        .find(|(_, character)| matches!(character, '<' | '>' | '=' | '!' | '~' | ';' | '[' | ' '))
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    &value[..end]
}

fn xml_element_range(text: &str, element: &str) -> Result<std::ops::Range<usize>> {
    let opening = format!("<{element}>");
    let closing = format!("</{element}>");
    let start = text
        .find(&opening)
        .map(|index| index + opening.len())
        .ok_or_else(|| Error::Validation(format!("XML element <{element}> was not found")))?;
    let end = text[start..]
        .find(&closing)
        .map(|index| start + index)
        .ok_or_else(|| Error::Validation(format!("XML element <{element}> is not closed")))?;
    Ok(start..end)
}

fn read_xml_element(text: &str, element: &str) -> Result<String> {
    let range = xml_element_range(text, element)?;
    Ok(text[range].trim().to_owned())
}

fn edit_xml_element(text: &str, element: &str, value: &str) -> Result<String> {
    let range = xml_element_range(text, element)?;
    let existing = &text[range.clone()];
    let leading = existing.len() - existing.trim_start().len();
    let trailing = existing.len() - existing.trim_end().len();
    let mut replacement = String::new();
    replacement.push_str(&existing[..leading]);
    replacement.push_str(value);
    replacement.push_str(&existing[existing.len() - trailing..]);
    let mut edited = text.to_owned();
    edited.replace_range(range, &replacement);
    Ok(edited)
}

fn go_module_line(text: &str) -> Option<(std::ops::Range<usize>, &str)> {
    let mut offset = 0;
    for line_with_newline in text.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let trimmed = line.trim();
        if let Some(module) = trimmed.strip_prefix("module ") {
            let module = module.trim();
            let start = offset + line.find(module)?;
            return Some((start..start + module.len(), module));
        }
        offset += line_with_newline.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_specialization_preserves_layout_and_range_styles() {
        let input = include_str!("../../tests/fixtures/ecosystems/package.json");
        let output = NpmAdapter
            .edit_version(input, "2.0.0")
            .and_then(|text| NpmAdapter.edit_dependency(&text, "sample-dependency", "2.0.0"))
            .and_then(|text| NpmAdapter.edit_dependency(&text, "sample-peer", "2.0.0"))
            .expect("npm edits");
        assert_eq!(NpmAdapter.name(&output).expect("name"), "sample-library");
        assert_eq!(NpmAdapter.version(&output).expect("version"), "2.0.0");
        assert!(output.contains("\"sample-dependency\": \"^2.0.0\""));
        assert!(output.contains("\"sample-peer\": \"workspace:^2.0.0\""));
    }

    #[test]
    fn cargo_specialization_preserves_comments_and_dependency_shapes() {
        let input = include_str!("../../tests/fixtures/ecosystems/Cargo.toml");
        let output = CargoAdapter
            .edit_version(input, "2.0.0")
            .and_then(|text| CargoAdapter.edit_dependency(&text, "sample-dependency", "2.0.0"))
            .and_then(|text| CargoAdapter.edit_dependency(&text, "sample-string", "2.0.0"))
            .and_then(|text| CargoAdapter.edit_dependency(&text, "sample-workspace", "2.0.0"))
            .expect("Cargo edits");
        assert_eq!(CargoAdapter.name(&output).expect("name"), "sample-library");
        assert_eq!(
            CargoAdapter.version(&output).expect("version"),
            Some("2.0.0".to_owned())
        );
        assert!(output.contains("version = \"2.0.0\" # version comment"));
        assert!(output.contains("version = \"2.0.0"));
        assert!(output.contains("# dependency comment"));
        assert!(output.contains("sample-string = \"2.0.0\""));
    }

    #[test]
    fn pub_and_python_edits_are_targeted() {
        let pubspec = "name: sample-library\nversion: 1.2.3 # keep\ndependencies:\n  sample-dependency: ^1.0.0\n";
        let pubspec = PubAdapter
            .edit_version(pubspec, "2.0.0")
            .and_then(|text| PubAdapter.edit_dependency(&text, "sample-dependency", "2.0.0"))
            .expect("Pub edits");
        assert!(pubspec.contains("version: 2.0.0 # keep"));
        assert!(pubspec.contains("sample-dependency: ^2.0.0"));

        let pyproject = "[project]\nname = \"sample-library\"\nversion = \"1.2.3\" # keep\ndependencies = [\"sample-dependency>=1.0.0\", \"other>=3\"]\n";
        let pyproject = PythonAdapter
            .edit_version(pyproject, "2.0.0")
            .and_then(|text| PythonAdapter.edit_dependency(&text, "sample-dependency", "2.0.0"))
            .expect("Python edits");
        assert!(pyproject.contains("version = \"2.0.0\" # keep"));
        assert!(pyproject.contains("sample-dependency>=2.0.0"));
        assert!(pyproject.contains("other>=3"));
    }

    #[test]
    fn msbuild_edit_preserves_xml_layout() {
        let project = "<Project>\n  <!-- keep -->\n  <PropertyGroup><PackageId>sample-library</PackageId><Version>1.2.3</Version></PropertyGroup>\n  <ItemGroup><PackageReference Include=\"sample-dependency\" Version=\"1.0.0\" /></ItemGroup>\n</Project>\n";
        let project = MsbuildAdapter
            .edit_version(project, "2.0.0")
            .and_then(|text| MsbuildAdapter.edit_dependency(&text, "sample-dependency", "2.0.0"))
            .expect("MSBuild edits");
        assert!(project.contains("<!-- keep -->"));
        assert!(project.contains("<Version>2.0.0</Version>"));
        assert!(project.contains("Version=\"2.0.0\""));
    }

    #[test]
    fn go_major_rewrites_only_module_path() {
        let module = "module example.invalid/sample/v2\n\ngo 1.22\n\nrequire example.invalid/dependency v1.0.0\n";
        let edited = GoAdapter
            .edit_major_module_path(module, "3.0.0")
            .expect("Go major edit");
        assert!(edited.starts_with("module example.invalid/sample/v3\n"));
        assert!(edited.contains("require example.invalid/dependency v1.0.0"));
    }
}
