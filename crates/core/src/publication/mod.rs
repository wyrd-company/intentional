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

/// GitHub host every Intentional release identity is published under.
const GITHUB_HOST: &str = "github.com";

/// Owner and repository the workspace's own origin remote names.
///
/// A document under verification can claim any repository it likes, so the
/// identity it is compared against is read from the verifying workspace rather
/// than taken from the document. That makes this the sole authority for the
/// comparison, so the host is parsed as the URL's authority rather than matched
/// anywhere in the string: a remote on another forge, or one whose path merely
/// contains the GitHub host, names no GitHub repository and must not resolve to
/// an owner and name that later reads are performed against.
pub(crate) fn origin_identity(root: &Path) -> Result<String> {
    let url = GitCommand::new(root)
        .args(["remote", "get-url", "origin"])
        .run()?
        .line()?;
    let refuse = || {
        Error::Validation(format!(
            "origin remote {url:?} does not name a repository on {GITHUB_HOST}"
        ))
    };
    let (authority, path) = split_authority(&url).ok_or_else(refuse)?;
    // A user info prefix is part of the authority and never part of the host.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // An explicit port is permitted; anything else after the host is not.
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    if !host.eq_ignore_ascii_case(GITHUB_HOST) {
        return Err(refuse());
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    match path.split('/').collect::<Vec<_>>().as_slice() {
        [owner, name] if !owner.is_empty() && !name.is_empty() => Ok(format!("{owner}/{name}")),
        _ => Err(refuse()),
    }
}

/// Split a Git remote URL into its authority and its repository path.
///
/// Git accepts both a scheme-bearing URL and the scp-like `host:path` short
/// form, and the two put the host in different places. Returning `None` for
/// anything else keeps a local path or a relative remote from being read as a
/// host that happens to compare equal.
fn split_authority(url: &str) -> Option<(&str, &str)> {
    if let Some((_, rest)) = url.split_once("://") {
        return rest.split_once('/');
    }
    let (authority, path) = url.split_once(':')?;
    // A Windows drive letter or a bare path is not an scp-form remote.
    (!authority.is_empty() && !authority.contains('/') && authority.len() > 1)
        .then_some((authority, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;

    /// Read the origin identity of a workspace whose remote is `url`.
    fn identity(label: &str, url: &str) -> Result<String> {
        let workspace = Workspace::new(label);
        let root = workspace.root();
        for arguments in [vec!["init", "-q"], vec!["remote", "add", "origin", url]] {
            GitCommand::new(root)
                .args(arguments)
                .run()
                .expect("git fixture command");
        }
        origin_identity(root)
    }

    #[test]
    fn reads_the_owner_and_repository_of_a_github_origin() {
        for (label, url) in [
            (
                "origin-scp",
                "git@github.com:sample-owner/sample-repository.git",
            ),
            (
                "origin-https",
                "https://github.com/sample-owner/sample-repository.git",
            ),
            (
                "origin-no-suffix",
                "https://github.com/sample-owner/sample-repository",
            ),
            (
                "origin-token",
                "https://x-access-token:secret@github.com/sample-owner/sample-repository.git",
            ),
            (
                "origin-ssh-url",
                "ssh://git@github.com/sample-owner/sample-repository.git",
            ),
        ] {
            assert_eq!(
                identity(label, url).expect("a GitHub origin resolves"),
                "sample-owner/sample-repository",
                "{url}"
            );
        }
    }

    #[test]
    fn refuses_an_origin_that_is_not_on_the_github_host() {
        // The second URL is the one that matters: its path contains the GitHub
        // host, so any match that is not anchored on the authority reads it as
        // a GitHub repository and later retrievals are performed against one.
        for (label, url) in [
            (
                "origin-other-forge",
                "git@forge.example.test:sample-owner/sample-repository.git",
            ),
            (
                "origin-embedded-host",
                "https://forge.example.test/github.com/sample-owner/sample-repository",
            ),
            (
                "origin-host-suffix",
                "https://notgithub.com/sample-owner/sample-repository",
            ),
            ("origin-local-path", "/srv/git/sample-repository.git"),
        ] {
            let error = identity(label, url).expect_err("a foreign origin is refused");
            assert!(
                error.to_string().contains("does not name a repository on"),
                "{url}: {error}"
            );
        }
    }
}
