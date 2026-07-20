// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Format-preserving manifest and ecosystem adapters.

pub mod ecosystem;
pub mod format;

pub use ecosystem::{
    CargoAdapter, GoAdapter, MsbuildAdapter, NpmAdapter, PubAdapter, PythonAdapter,
};
pub use format::{FormatAdapter, JsonFormat, TomlFormat, YamlFormat};
