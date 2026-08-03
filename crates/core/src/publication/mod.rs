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

use crate::error::{Error, Result};
use crate::release::git::GitCommand;
use std::path::Path;

/// Owner and repository the workspace's own origin remote names.
///
/// A document under verification can claim any repository it likes, so the
/// identity it is compared against is read from the verifying workspace rather
/// than taken from the document.
pub(crate) fn origin_identity(root: &Path) -> Result<String> {
    let url = GitCommand::new(root)
        .args(["remote", "get-url", "origin"])
        .run()?
        .line()?;
    let path = url
        .rsplit_once(':')
        .map_or(url.as_str(), |(_, path)| path)
        .rsplit("github.com/")
        .next()
        .unwrap_or_default()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let segments = path.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        [owner, name] if !owner.is_empty() && !name.is_empty() => Ok(format!("{owner}/{name}")),
        _ => Err(Error::Validation(format!(
            "origin remote {url:?} does not name a GitHub owner/repository"
        ))),
    }
}
