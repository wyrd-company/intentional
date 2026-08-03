// ---
// relationships:
//   implements: github-release-executor
// ---

//! Provider-neutral publication protocol.
//!
//! Publication is resumable rather than transactional. A destination is
//! observed rather than trusted, an observation is bounded by its adapter's
//! eventual-consistency policy, and the resulting evidence is sealed into an
//! after-publication tag so a later retry reuses the historical observation
//! instead of rewriting it.

pub mod draft;
pub mod observation;
pub mod release;
pub mod verify;
