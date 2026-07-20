// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Thin npm and Cargo specializations over JSON and TOML format adapters.

use super::format::{FormatAdapter, JsonFormat, TomlFormat};
use crate::error::{Error, Result};
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
}
