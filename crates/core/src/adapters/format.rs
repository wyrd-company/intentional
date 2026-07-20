// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Format adapters that replace only a configured version value.

use crate::error::{Error, Result};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use toml_edit::{value, DocumentMut, Item};

/// A format-preserving version-value adapter.
pub trait FormatAdapter {
    /// Read a string value selected by `pointer`.
    fn read(&self, file: &Path, pointer: &str) -> Result<String> {
        let text = std::fs::read_to_string(file).map_err(|error| Error::io(file, error))?;
        self.read_text(&text, pointer)
    }

    /// Produce updated text without touching the filesystem.
    fn edit_text(&self, text: &str, pointer: &str, value: &str) -> Result<String>;

    /// Read a selected string from in-memory text.
    fn read_text(&self, text: &str, pointer: &str) -> Result<String>;

    /// Write a selected value unless `dry_run` is enabled.
    fn write(&self, file: &Path, pointer: &str, new_value: &str, dry_run: bool) -> Result<()> {
        let text = std::fs::read_to_string(file).map_err(|error| Error::io(file, error))?;
        let edited = self.edit_text(&text, pointer, new_value)?;
        if !dry_run && edited != text {
            std::fs::write(file, edited).map_err(|error| Error::io(file, error))?;
        }
        Ok(())
    }
}

/// Targeted JSON string adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonFormat;

/// Comment-preserving TOML adapter backed by `toml_edit`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TomlFormat;

/// Targeted YAML scalar-line adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct YamlFormat;

impl FormatAdapter for JsonFormat {
    fn edit_text(&self, text: &str, pointer: &str, new_value: &str) -> Result<String> {
        let node = JsonParser::new(text).parse()?;
        let target = node.find(&json_pointer(pointer)?)?;
        let mut edited = text.to_owned();
        let replacement = serde_json::to_string(new_value)
            .map_err(|error| Error::Validation(format!("JSON value is invalid: {error}")))?;
        edited.replace_range(target.range.clone(), &replacement);
        Ok(edited)
    }

    fn read_text(&self, text: &str, pointer: &str) -> Result<String> {
        let node = JsonParser::new(text).parse()?;
        let target = node.find(&json_pointer(pointer)?)?;
        serde_json::from_str(&text[target.range.clone()]).map_err(|error| {
            Error::Validation(format!(
                "JSON pointer {pointer} does not select a string: {error}"
            ))
        })
    }
}

impl FormatAdapter for TomlFormat {
    fn edit_text(&self, text: &str, pointer: &str, new_value: &str) -> Result<String> {
        let mut document = text
            .parse::<DocumentMut>()
            .map_err(|error| Error::Validation(format!("invalid TOML: {error}")))?;
        let item = toml_item_mut(document.as_item_mut(), &key_pointer(pointer)?, pointer)?;
        let decor = item.as_value().map(|old| old.decor().clone());
        *item = value(new_value);
        if let (Some(decor), Some(new)) = (decor, item.as_value_mut()) {
            *new.decor_mut() = decor;
        }
        Ok(document.to_string())
    }

    fn read_text(&self, text: &str, pointer: &str) -> Result<String> {
        let document = text
            .parse::<DocumentMut>()
            .map_err(|error| Error::Validation(format!("invalid TOML: {error}")))?;
        let item = toml_item(document.as_item(), &key_pointer(pointer)?, pointer)?;
        item.as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Validation(format!("TOML pointer {pointer} is not a string")))
    }
}

impl FormatAdapter for YamlFormat {
    fn edit_text(&self, text: &str, pointer: &str, new_value: &str) -> Result<String> {
        let keys = key_pointer(pointer)?;
        let line = find_yaml_line(text, &keys, pointer)?;
        let source = &text[line.clone()];
        let colon = source
            .find(':')
            .ok_or_else(|| Error::Validation(format!("invalid YAML line for {pointer}")))?;
        let after_colon = &source[colon + 1..];
        let value_offset = after_colon.len() - after_colon.trim_start().len();
        let value_start = colon + 1 + value_offset;
        let value_end = yaml_value_end(source, value_start);
        let old = source[value_start..value_end].trim_end();
        let replacement = quote_like_yaml(old, new_value);
        let mut new_line = String::new();
        new_line.push_str(&source[..value_start]);
        new_line.push_str(&replacement);
        new_line.push_str(&source[value_end..]);
        let mut edited = text.to_owned();
        edited.replace_range(line, &new_line);
        Ok(edited)
    }

    fn read_text(&self, text: &str, pointer: &str) -> Result<String> {
        let keys = key_pointer(pointer)?;
        let line = find_yaml_line(text, &keys, pointer)?;
        let source = &text[line];
        let colon = source
            .find(':')
            .ok_or_else(|| Error::Validation(format!("invalid YAML line for {pointer}")))?;
        let start = colon + 1 + source[colon + 1..].len() - source[colon + 1..].trim_start().len();
        let end = yaml_value_end(source, start);
        let raw = source[start..end].trim();
        Ok(unquote_yaml(raw).to_owned())
    }
}

#[derive(Debug)]
struct JsonNode {
    range: Range<usize>,
    kind: JsonKind,
}

#[derive(Debug)]
enum JsonKind {
    Object(BTreeMap<String, JsonNode>),
    Array(Vec<JsonNode>),
    Scalar,
}

impl JsonNode {
    fn find(&self, pointer: &[String]) -> Result<&Self> {
        let mut current = self;
        for segment in pointer {
            current = match &current.kind {
                JsonKind::Object(values) => values.get(segment),
                JsonKind::Array(values) => segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| values.get(index)),
                JsonKind::Scalar => None,
            }
            .ok_or_else(|| {
                Error::Validation(format!("JSON pointer segment {segment:?} was not found"))
            })?;
        }
        Ok(current)
    }
}

struct JsonParser<'a> {
    text: &'a str,
    cursor: usize,
}

impl<'a> JsonParser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, cursor: 0 }
    }

    fn parse(mut self) -> Result<JsonNode> {
        let node = self.value()?;
        self.whitespace();
        if self.cursor != self.text.len() {
            return Err(Error::Validation("trailing content in JSON".to_owned()));
        }
        Ok(node)
    }

    fn value(&mut self) -> Result<JsonNode> {
        self.whitespace();
        let start = self.cursor;
        let byte = self.peek().ok_or_else(|| {
            Error::Validation("unexpected end of JSON while reading value".to_owned())
        })?;
        let kind = match byte {
            b'{' => self.object()?,
            b'[' => self.array()?,
            b'"' => {
                self.string()?;
                JsonKind::Scalar
            }
            _ => {
                while let Some(byte) = self.peek() {
                    if byte.is_ascii_whitespace() || b",]}".contains(&byte) {
                        break;
                    }
                    self.cursor += 1;
                }
                if self.cursor == start {
                    return Err(Error::Validation("invalid JSON scalar".to_owned()));
                }
                JsonKind::Scalar
            }
        };
        Ok(JsonNode {
            range: start..self.cursor,
            kind,
        })
    }

    fn object(&mut self) -> Result<JsonKind> {
        self.expect(b'{')?;
        self.whitespace();
        let mut values = BTreeMap::new();
        if self.take(b'}') {
            return Ok(JsonKind::Object(values));
        }
        loop {
            self.whitespace();
            let key_range = self.string()?;
            let key: String = serde_json::from_str(&self.text[key_range])
                .map_err(|error| Error::Validation(format!("invalid JSON object key: {error}")))?;
            self.whitespace();
            self.expect(b':')?;
            let value = self.value()?;
            values.insert(key, value);
            self.whitespace();
            if self.take(b'}') {
                break;
            }
            self.expect(b',')?;
        }
        Ok(JsonKind::Object(values))
    }

    fn array(&mut self) -> Result<JsonKind> {
        self.expect(b'[')?;
        self.whitespace();
        let mut values = Vec::new();
        if self.take(b']') {
            return Ok(JsonKind::Array(values));
        }
        loop {
            values.push(self.value()?);
            self.whitespace();
            if self.take(b']') {
                break;
            }
            self.expect(b',')?;
        }
        Ok(JsonKind::Array(values))
    }

    fn string(&mut self) -> Result<Range<usize>> {
        let start = self.cursor;
        self.expect(b'"')?;
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            self.cursor += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Ok(start..self.cursor);
            }
        }
        Err(Error::Validation("unterminated JSON string".to_owned()))
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.cursor).copied()
    }

    fn take(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<()> {
        if self.take(byte) {
            Ok(())
        } else {
            Err(Error::Validation(format!(
                "expected JSON byte {:?} at offset {}",
                byte as char, self.cursor
            )))
        }
    }
}

fn json_pointer(pointer: &str) -> Result<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let rest = pointer
        .strip_prefix('/')
        .ok_or_else(|| Error::Validation(format!("JSON pointer must start with '/': {pointer}")))?;
    Ok(rest
        .split('/')
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect())
}

fn key_pointer(pointer: &str) -> Result<Vec<String>> {
    let parts = if let Some(rest) = pointer.strip_prefix('/') {
        rest.split('/').collect::<Vec<_>>()
    } else {
        pointer.split('.').collect::<Vec<_>>()
    };
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(Error::Validation(format!("invalid key pointer {pointer}")));
    }
    Ok(parts.into_iter().map(str::to_owned).collect())
}

fn toml_item_mut<'a>(
    mut item: &'a mut Item,
    keys: &[String],
    pointer: &str,
) -> Result<&'a mut Item> {
    for key in keys {
        item = item
            .get_mut(key)
            .ok_or_else(|| Error::Validation(format!("TOML pointer {pointer} was not found")))?;
    }
    Ok(item)
}

fn toml_item<'a>(mut item: &'a Item, keys: &[String], pointer: &str) -> Result<&'a Item> {
    for key in keys {
        item = item
            .get(key)
            .ok_or_else(|| Error::Validation(format!("TOML pointer {pointer} was not found")))?;
    }
    Ok(item)
}

fn find_yaml_line(text: &str, keys: &[String], pointer: &str) -> Result<Range<usize>> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut offset = 0;
    for line_with_newline in text.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            offset += line_with_newline.len();
            continue;
        }
        let indent = line.len() - trimmed.len();
        let Some(colon) = trimmed.find(':') else {
            offset += line_with_newline.len();
            continue;
        };
        let key = trimmed[..colon].trim().trim_matches(['\'', '"']).to_owned();
        while stack.last().is_some_and(|(prior, _)| *prior >= indent) {
            stack.pop();
        }
        let mut path = stack
            .iter()
            .map(|(_, key)| key.as_str())
            .collect::<Vec<_>>();
        path.push(&key);
        if path == keys.iter().map(String::as_str).collect::<Vec<_>>() {
            return Ok(offset..offset + line.len());
        }
        if trimmed[colon + 1..].trim().is_empty() {
            stack.push((indent, key));
        }
        offset += line_with_newline.len();
    }
    Err(Error::Validation(format!(
        "YAML pointer {pointer} was not found"
    )))
}

fn yaml_value_end(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let quote = bytes
        .get(start)
        .copied()
        .filter(|byte| matches!(byte, b'\'' | b'"'));
    let mut index = start;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(quote) = quote {
            if index > start && byte == quote && (!escaped || quote == b'\'') {
                index += 1;
                break;
            }
            escaped = byte == b'\\' && !escaped;
            if byte != b'\\' {
                escaped = false;
            }
        } else if byte == b'#' && (index == start || bytes[index - 1].is_ascii_whitespace()) {
            break;
        }
        index += 1;
    }
    while index > start && bytes[index - 1].is_ascii_whitespace() {
        index -= 1;
    }
    index
}

fn quote_like_yaml(old: &str, value: &str) -> String {
    if old.starts_with('\'') && old.ends_with('\'') {
        format!("'{}'", value.replace('\'', "''"))
    } else if old.starts_with('"') && old.ends_with('"') {
        serde_json::to_string(value).expect("string serialization cannot fail")
    } else {
        value.to_owned()
    }
}

fn unquote_yaml(raw: &str) -> &str {
    if raw.len() >= 2
        && ((raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('"') && raw.ends_with('"')))
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_targeted_edit_preserves_odd_indentation() {
        let input = include_str!("../../tests/fixtures/formats/odd.json");
        let output = JsonFormat
            .edit_text(input, "/metadata/version", "2.0.0")
            .expect("JSON edit");
        assert_eq!(output, input.replacen("\"1.2.3\"", "\"2.0.0\"", 1));
        assert_eq!(
            JsonFormat
                .read_text(&output, "/metadata/version")
                .expect("JSON read"),
            "2.0.0"
        );
    }

    #[test]
    fn toml_edit_preserves_comments_and_spacing() {
        let input = include_str!("../../tests/fixtures/formats/comments.toml");
        let output = TomlFormat
            .edit_text(input, "/package/version", "2.0.0")
            .expect("TOML edit");
        assert!(output.contains("# Package comment"));
        assert!(output.contains("version = \"2.0.0\" # keep this comment"));
        assert!(output.contains("version = \"9.9.9\""));
        assert_eq!(
            TomlFormat
                .read_text(&output, "package.version")
                .expect("TOML read"),
            "2.0.0"
        );
    }

    #[test]
    fn yaml_edit_preserves_comments_quotes_and_other_keys() {
        let input = include_str!("../../tests/fixtures/formats/comments.yaml");
        let output = YamlFormat
            .edit_text(input, "/package/version", "2.0.0")
            .expect("YAML edit");
        assert!(output.contains("version: '2.0.0' # keep this comment"));
        assert!(output.contains("version: \"9.9.9\""));
        assert_eq!(
            YamlFormat
                .read_text(&output, "/package/version")
                .expect("YAML read"),
            "2.0.0"
        );
    }
}
