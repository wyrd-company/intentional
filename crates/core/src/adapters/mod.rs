// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Format-preserving manifest and ecosystem adapters.

pub mod format;

pub use format::{FormatAdapter, JsonFormat, TomlFormat, YamlFormat};
