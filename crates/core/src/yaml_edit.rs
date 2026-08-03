// ---
// relationships:
//   implements: github-release-executor
// ---

//! Format-preserving edits to block-style YAML documents.
//!
//! Serializing a parsed model back over a user's file destroys their comments,
//! key order, and formatting, and materializes defaults they never wrote. This
//! editor instead splices rendered text into the exact region a key occupies,
//! so every byte outside that region survives the edit.
//!
//! Block mappings are edited in place. A flow-style or scalar container cannot
//! carry a nested edit, so the editor rebuilds that one container from its
//! parsed value and replaces it as a block mapping. The rebuild is confined to
//! the container that could not be traversed; the rest of the document is
//! untouched.

use crate::error::{Error, Result};
use serde_yaml::{Mapping, Value};
use std::ops::Range;

/// A YAML document edited without disturbing unrelated formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    text: String,
    newline: &'static str,
}

impl Document {
    /// Parse a document, rejecting YAML the editor cannot interpret.
    pub fn parse(text: &str) -> Result<Self> {
        serde_yaml::from_str::<Value>(text)?;
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        Ok(Self {
            text: text.to_owned(),
            newline,
        })
    }

    /// Current document text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Consume the editor and return its text.
    pub fn into_text(self) -> String {
        self.text
    }

    /// Parsed value of the whole document.
    pub fn value(&self) -> Result<Value> {
        Ok(serde_yaml::from_str(&self.text)?)
    }

    /// Value at a mapping path, or `None` when the path is absent.
    pub fn get(&self, path: &[&str]) -> Result<Option<Value>> {
        let mut current = self.value()?;
        for segment in path {
            let Some(mapping) = current.as_mapping() else {
                return Ok(None);
            };
            let Some(next) = mapping.get(Value::String((*segment).to_owned())) else {
                return Ok(None);
            };
            current = next.clone();
        }
        Ok(Some(current))
    }

    /// Keys of the mapping at a path, in document order.
    pub fn keys(&self, path: &[&str]) -> Result<Vec<String>> {
        let Some(value) = self.get(path)? else {
            return Ok(Vec::new());
        };
        let Some(mapping) = value.as_mapping() else {
            return Ok(Vec::new());
        };
        Ok(mapping
            .keys()
            .filter_map(|key| key.as_str().map(str::to_owned))
            .collect())
    }

    /// Set a mapping path to a value, creating intermediate mappings as needed.
    pub fn set(&mut self, path: &[&str], value: &Value) -> Result<()> {
        let (key, indent, placement) = match self.locate(path)? {
            Located::Entry(entry) => (
                path[path.len() - 1].to_owned(),
                entry.indent,
                Placement::Replace(entry.key_start..entry.end),
            ),
            Located::Missing {
                depth,
                region,
                indent,
            } => (
                path[depth].to_owned(),
                indent,
                Placement::Insert(region, nested(&path[depth + 1..], value)),
            ),
            Located::Unsupported { depth } => {
                let mut container = self.get(&path[..=depth])?.unwrap_or(Value::Null);
                assign(&mut container, &path[depth + 1..], value.clone());
                return self.set(&path[..=depth], &container);
            }
        };
        match placement {
            Placement::Replace(range) => {
                let rendered = self.render_entry(&key, value, indent);
                self.text.replace_range(range, &rendered);
            }
            Placement::Insert(at, nested) => {
                let rendered = self.render_entry(&key, &nested, indent);
                self.text.insert_str(at, &rendered);
            }
        }
        Ok(())
    }

    /// Remove a mapping path with its attached comments; reports whether it existed.
    pub fn remove(&mut self, path: &[&str]) -> Result<bool> {
        match self.locate(path)? {
            Located::Entry(entry) => {
                let end = with_trailing_blanks(&self.text, entry.end);
                self.text.replace_range(entry.lead..end, "");
                Ok(true)
            }
            Located::Missing { .. } => Ok(false),
            Located::Unsupported { depth } => {
                let Some(mut container) = self.get(&path[..=depth])? else {
                    return Ok(false);
                };
                if !unassign(&mut container, &path[depth + 1..]) {
                    return Ok(false);
                }
                self.set(&path[..=depth], &container)?;
                Ok(true)
            }
        }
    }

    fn locate(&self, path: &[&str]) -> Result<Located> {
        if path.is_empty() {
            return Err(Error::Validation(
                "a yaml edit path must name at least one key".to_owned(),
            ));
        }
        let mut region = 0..self.text.len();
        let mut indent = root_indent(&self.text);
        for (depth, segment) in path.iter().enumerate() {
            let Some(entries) = entries(&self.text, region.clone(), indent) else {
                if depth == 0 {
                    return Err(Error::Validation(
                        "the document root is not a block mapping".to_owned(),
                    ));
                }
                return Ok(Located::Unsupported { depth: depth - 1 });
            };
            let Some(entry) = entries.iter().find(|entry| entry.key == **segment) else {
                let at = entries
                    .last()
                    .map_or(region.start, |entry| entry.end.min(region.end));
                return Ok(Located::Missing {
                    depth,
                    region: at,
                    indent,
                });
            };
            if depth + 1 == path.len() {
                return Ok(Located::Entry(entry.clone()));
            }
            let Some(child) = entry.block_value(&self.text) else {
                return Ok(Located::Unsupported { depth });
            };
            region = child.0;
            indent = child.1;
        }
        unreachable!("the loop returns on the final path segment")
    }

    fn render_entry(&self, key: &str, value: &Value, indent: usize) -> String {
        let mut rendered = String::new();
        emit_entry(&mut rendered, key, value, indent, self.newline);
        rendered
    }
}

enum Placement {
    Replace(Range<usize>),
    Insert(usize, Value),
}

enum Located {
    /// The path names an existing entry.
    Entry(Entry),
    /// The container at `depth` exists and `path[depth]` is absent from it.
    Missing {
        depth: usize,
        region: usize,
        indent: usize,
    },
    /// The container at `depth` cannot carry a nested block edit.
    Unsupported { depth: usize },
}

/// One key and the exact text region it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: String,
    /// Start of the comment block attached to this key.
    lead: usize,
    /// Start of the key's own line.
    key_start: usize,
    /// End of the key's content, excluding trailing blank separator lines.
    end: usize,
    /// Column the key starts in.
    indent: usize,
    /// Offset just past `key:` on the key line.
    value_start: usize,
    /// End of the key line, including its newline.
    key_line_end: usize,
}

impl Entry {
    /// Region and indent of a block value written under the key line.
    fn block_value(&self, text: &str) -> Option<(Range<usize>, usize)> {
        let inline = text[self.value_start..self.key_line_end].trim();
        if !inline.is_empty() && !inline.starts_with('#') {
            return None;
        }
        let region = self.key_line_end..self.end;
        let indent = significant_indent(text, region.clone())?;
        (indent > self.indent).then_some((region, indent))
    }
}

fn root_indent(text: &str) -> usize {
    significant_indent(text, 0..text.len()).unwrap_or(0)
}

/// Indent of the first line in `region` that is neither blank nor a comment.
fn significant_indent(text: &str, region: Range<usize>) -> Option<usize> {
    lines(text, region).find_map(|line| {
        let content = &text[line.clone()];
        (!is_ignorable(content)).then(|| indent_of(content))
    })
}

/// Entries of the block mapping occupying `region` at `indent`.
///
/// Returns `None` when the region holds a sequence, a scalar, or nothing the
/// editor can address by key.
fn entries(text: &str, region: Range<usize>, indent: usize) -> Option<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending_comment: Option<usize> = None;
    for line in lines(text, region.clone()) {
        let content = &text[line.clone()];
        if content.trim().is_empty() {
            continue;
        }
        if content.trim_start().starts_with('#') {
            if indent_of(content) >= indent {
                pending_comment.get_or_insert(line.start);
            }
            continue;
        }
        let line_indent = indent_of(content);
        if line_indent > indent {
            pending_comment = None;
            continue;
        }
        if line_indent < indent {
            break;
        }
        let Some((key, value_start)) = split_key(content) else {
            // A sequence item at the container indent means this region is not
            // a mapping the editor can address by key.
            if entries.is_empty() {
                return None;
            }
            break;
        };
        if let Some(previous) = entries.last_mut() {
            previous.end = trimmed_end(
                text,
                previous.key_line_end,
                pending_comment.unwrap_or(line.start),
                indent,
            );
        }
        entries.push(Entry {
            key,
            lead: pending_comment.take().unwrap_or(line.start),
            key_start: line.start,
            end: line.end,
            indent,
            value_start: line.start + value_start,
            key_line_end: line.end,
        });
    }
    if let Some(previous) = entries.last_mut() {
        previous.end = trimmed_end(text, previous.key_line_end, region.end, indent);
    }
    Some(entries)
}

/// Pull an entry's end back past trailing lines that belong to its container.
///
/// Blank separators and comments written outside the entry's indentation
/// introduce or describe what follows, so an edit to the entry must leave them
/// where the author put them.
fn trimmed_end(text: &str, floor: usize, mut end: usize, indent: usize) -> usize {
    while end > floor {
        let start = line_start(text, end - 1);
        if start < floor {
            break;
        }
        let line = &text[start..end];
        let outside = line.trim().is_empty()
            || (line.trim_start().starts_with('#') && indent_of(line) < indent);
        if !outside {
            break;
        }
        end = start;
    }
    end.max(floor)
}

/// Extend a removal over the blank separator lines the entry left behind.
fn with_trailing_blanks(text: &str, mut end: usize) -> usize {
    for line in lines(text, end..text.len()) {
        if !text[line.clone()].trim().is_empty() {
            break;
        }
        end = line.end;
    }
    end
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |index| index + 1)
}

fn lines(text: &str, region: Range<usize>) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut cursor = region.start;
    std::iter::from_fn(move || {
        if cursor >= region.end {
            return None;
        }
        let end = text[cursor..region.end]
            .find('\n')
            .map_or(region.end, |index| cursor + index + 1);
        let line = cursor..end;
        cursor = end;
        Some(line)
    })
}

fn is_ignorable(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn indent_of(content: &str) -> usize {
    content.len() - content.trim_start_matches(' ').len()
}

/// Split `key:` from a block mapping line, returning the key and the offset after the colon.
fn split_key(content: &str) -> Option<(String, usize)> {
    let line = content.trim_end_matches(['\n', '\r']);
    let start = indent_of(content);
    let body = &line[start..];
    if body.starts_with('-') {
        return None;
    }
    let (key, offset) = if let Some(quote) = body.chars().next().filter(|c| *c == '"' || *c == '\'')
    {
        let closing = body[1..].find(quote)? + 1;
        (body[1..closing].to_owned(), closing + 1)
    } else {
        let colon = body.find(':')?;
        (body[..colon].trim_end().to_owned(), colon)
    };
    if !body[offset..].starts_with(':') {
        return None;
    }
    let after = offset + 1;
    if !body[after..].is_empty() && !body[after..].starts_with(' ') {
        return None;
    }
    Some((key, start + after))
}

fn nested(path: &[&str], value: &Value) -> Value {
    path.iter().rev().fold(value.clone(), |inner, key| {
        let mut mapping = Mapping::new();
        mapping.insert(Value::String((*key).to_owned()), inner);
        Value::Mapping(mapping)
    })
}

fn assign(container: &mut Value, path: &[&str], value: Value) {
    let Some((head, rest)) = path.split_first() else {
        *container = value;
        return;
    };
    if !container.is_mapping() {
        *container = Value::Mapping(Mapping::new());
    }
    let mapping = container.as_mapping_mut().expect("mapping");
    let key = Value::String((*head).to_owned());
    let entry = mapping.entry(key).or_insert(Value::Null);
    assign(entry, rest, value);
}

fn unassign(container: &mut Value, path: &[&str]) -> bool {
    let Some((head, rest)) = path.split_first() else {
        return false;
    };
    let Some(mapping) = container.as_mapping_mut() else {
        return false;
    };
    let key = Value::String((*head).to_owned());
    if rest.is_empty() {
        return mapping.remove(&key).is_some();
    }
    mapping
        .get_mut(&key)
        .is_some_and(|nested| unassign(nested, rest))
}

fn emit_entry(out: &mut String, key: &str, value: &Value, indent: usize, newline: &str) {
    let spaces = " ".repeat(indent);
    let rendered_key = emit_key(key);
    match inline_scalar(value) {
        Some(scalar) if scalar.is_empty() => {
            out.push_str(&format!("{spaces}{rendered_key}:{newline}"));
        }
        Some(scalar) => {
            out.push_str(&format!("{spaces}{rendered_key}: {scalar}{newline}"));
        }
        None => match block_scalar(value, indent + 2, newline) {
            Some(block) => {
                out.push_str(&format!("{spaces}{rendered_key}: {block}"));
            }
            None => {
                out.push_str(&format!("{spaces}{rendered_key}:{newline}"));
                emit_block(out, value, indent + 2, newline);
            }
        },
    }
}

fn emit_block(out: &mut String, value: &Value, indent: usize, newline: &str) {
    let spaces = " ".repeat(indent);
    match value {
        Value::Mapping(mapping) => {
            for (key, item) in mapping {
                let key = key.as_str().unwrap_or_default();
                emit_entry(out, key, item, indent, newline);
            }
        }
        Value::Sequence(items) => {
            for item in items {
                match inline_scalar(item) {
                    Some(scalar) if scalar.is_empty() => {
                        out.push_str(&format!("{spaces}-{newline}"));
                    }
                    Some(scalar) => out.push_str(&format!("{spaces}- {scalar}{newline}")),
                    None => {
                        let mut nested = String::new();
                        emit_block(&mut nested, item, indent + 2, newline);
                        out.push_str(&format!("{spaces}- {}", &nested[indent + 2..]));
                    }
                }
            }
        }
        _ => unreachable!("scalars are emitted inline"),
    }
}

/// Inline rendering of a value that occupies no additional lines.
fn inline_scalar(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::String(text) if text.contains('\n') => None,
        Value::String(text) => Some(emit_string(text)),
        Value::Mapping(mapping) if mapping.is_empty() => Some("{}".to_owned()),
        Value::Sequence(items) if items.is_empty() => Some("[]".to_owned()),
        _ => None,
    }
}

/// Literal block scalar for a multi-line string.
fn block_scalar(value: &Value, indent: usize, newline: &str) -> Option<String> {
    let Value::String(text) = value else {
        return None;
    };
    let spaces = " ".repeat(indent);
    let (body, header) = match text.strip_suffix('\n') {
        Some(body) if !body.ends_with('\n') => (body, "|"),
        _ => (text.as_str(), "|-"),
    };
    let mut rendered = format!("{header}{newline}");
    for line in body.split('\n') {
        if line.is_empty() {
            rendered.push_str(newline);
        } else {
            rendered.push_str(&format!("{spaces}{line}{newline}"));
        }
    }
    Some(rendered)
}

fn emit_string(text: &str) -> String {
    let rendered = serde_yaml::to_string(&Value::String(text.to_owned()))
        .unwrap_or_else(|_| format!("{text:?}"));
    rendered.trim_end_matches('\n').to_owned()
}

fn emit_key(key: &str) -> String {
    let plain = !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./".contains(character))
        && !key.chars().next().is_some_and(|c| c.is_ascii_digit());
    if plain {
        key.to_owned()
    } else {
        emit_string(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> Value {
        serde_yaml::from_str(text).expect("fixture value")
    }

    #[test]
    fn replaces_one_entry_and_preserves_every_other_byte() {
        let source = "# top comment\nname: release\n\n# owned by the repository\njobs:\n  test:\n    runs-on: ubuntu-latest\n  managed:\n    runs-on: ubuntu-latest\n\n# trailing note\n";
        let mut document = Document::parse(source).expect("parses");
        document
            .set(
                &["jobs", "managed"],
                &value("runs-on: ubuntu-24.04\nsteps:\n  - run: 'true'\n"),
            )
            .expect("replaces");
        assert_eq!(
            document.text(),
            "# top comment\nname: release\n\n# owned by the repository\njobs:\n  test:\n    runs-on: ubuntu-latest\n  managed:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: 'true'\n\n# trailing note\n"
        );
    }

    #[test]
    fn inserts_a_missing_nested_path_without_touching_neighbors() {
        let source = "contract: contract-1\nrelease-units:\n  component:\n    # keep this\n    path: component\n";
        let mut document = Document::parse(source).expect("parses");
        document
            .set(
                &["github", "workflows", "release", "path"],
                &value("'.github/workflows/release.yml'"),
            )
            .expect("inserts");
        assert_eq!(
            document.text(),
            "contract: contract-1\nrelease-units:\n  component:\n    # keep this\n    path: component\ngithub:\n  workflows:\n    release:\n      path: .github/workflows/release.yml\n"
        );
    }

    #[test]
    fn removes_an_entry_with_its_attached_comment() {
        let source = "jobs:\n  keep:\n    runs-on: ubuntu-latest\n\n  # describes the managed job\n  managed:\n    runs-on: ubuntu-latest\n\n  also-keep:\n    runs-on: ubuntu-latest\n";
        let mut document = Document::parse(source).expect("parses");
        assert!(document.remove(&["jobs", "managed"]).expect("removes"));
        assert_eq!(
            document.text(),
            "jobs:\n  keep:\n    runs-on: ubuntu-latest\n\n  also-keep:\n    runs-on: ubuntu-latest\n"
        );
        assert!(!document.remove(&["jobs", "absent"]).expect("absent key"));
    }

    #[test]
    fn rebuilds_only_the_flow_container_it_cannot_traverse() {
        let source = "name: release\non: { workflow_dispatch: {} }\n# repository note\njobs:\n  test: { runs-on: ubuntu-latest }\n";
        let mut document = Document::parse(source).expect("parses");
        document
            .set(&["on", "push", "tags"], &value("[ '*' ]"))
            .expect("sets nested flow path");
        assert_eq!(
            document.text(),
            "name: release\non:\n  workflow_dispatch: {}\n  push:\n    tags:\n      - '*'\n# repository note\njobs:\n  test: { runs-on: ubuntu-latest }\n"
        );
    }

    #[test]
    fn emits_multi_line_commands_as_literal_block_scalars() {
        let mut document =
            Document::parse("jobs:\n  managed:\n    runs-on: ubuntu-latest\n").expect("parses");
        let mut step = Mapping::new();
        step.insert(
            Value::String("run".to_owned()),
            Value::String("first\nsecond\n".to_owned()),
        );
        document
            .set(
                &["jobs", "managed", "steps"],
                &Value::Sequence(vec![Value::Mapping(step)]),
            )
            .expect("sets steps");
        assert_eq!(
            document.text(),
            "jobs:\n  managed:\n    runs-on: ubuntu-latest\n    steps:\n      - run: |\n          first\n          second\n"
        );
        assert!(document.value().is_ok(), "the result stays valid YAML");
    }

    #[test]
    fn round_trips_an_unmodified_document_byte_for_byte() {
        let source = "# header\nname: release\n\non:\n  workflow_dispatch:\n\njobs:\n  test:\n    runs-on: ubuntu-latest # trailing comment\n";
        let document = Document::parse(source).expect("parses");
        assert_eq!(document.into_text(), source);
    }

    #[test]
    fn reports_keys_and_values_without_mutating() {
        let document = Document::parse("jobs:\n  first: {}\n  second: {}\n").expect("parses");
        assert_eq!(
            document.keys(&["jobs"]).expect("keys"),
            vec!["first".to_owned(), "second".to_owned()]
        );
        assert_eq!(
            document.get(&["jobs", "first"]).expect("value"),
            Some(Value::Mapping(Mapping::new()))
        );
        assert_eq!(document.get(&["jobs", "absent"]).expect("absent"), None);
    }

    #[test]
    fn rejects_a_document_whose_root_is_not_a_block_mapping() {
        let mut document = Document::parse("- one\n- two\n").expect("parses");
        assert!(document
            .set(&["key"], &Value::Null)
            .expect_err("sequence root rejected")
            .to_string()
            .contains("not a block mapping"));
    }
}
