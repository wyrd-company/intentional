// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Core model and release operations for `itentional`.

/// Version of the core release model.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_version_is_0_1_0() {
        assert_eq!(super::VERSION, "0.1.0");
    }
}
