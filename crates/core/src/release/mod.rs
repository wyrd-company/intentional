// ---
// relationships:
//   implements: github-release-executor
// ---

//! Release preparation, candidate handoff, and independent handoff verification.

pub mod build;
pub mod candidate;
pub(crate) mod git;
pub mod prepare;
pub mod tag;
pub mod verify;
