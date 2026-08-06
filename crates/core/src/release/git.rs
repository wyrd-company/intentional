// ---
// relationships:
//   implements: github-release-executor
// ---

//! Bounded `git` invocations used by release preparation and handoff verification.
//!
//! Release preparation and verification need plumbing that `gix` does not
//! expose, notably index-free tree construction, tag object creation, and Git
//! bundle transport. Every invocation here is explicit, argument-only, and
//! never interpolates untrusted values into a shell.

use crate::error::{Error, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// One `git` invocation and its captured result.
pub(crate) struct GitCommand<'a> {
    directory: &'a Path,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    stdin: Option<Vec<u8>>,
}

/// Captured output of a completed `git` invocation.
pub(crate) struct GitOutput {
    /// Raw standard output.
    pub(crate) stdout: Vec<u8>,
    /// Raw standard error.
    pub(crate) stderr: Vec<u8>,
    /// Process exit status code, when the process was not signalled.
    pub(crate) code: Option<i32>,
}

impl GitOutput {
    /// Standard output as UTF-8 with trailing newlines removed.
    pub(crate) fn line(&self) -> Result<String> {
        let text = std::str::from_utf8(&self.stdout)
            .map_err(|error| Error::Git(format!("git produced invalid UTF-8 output: {error}")))?;
        Ok(text.trim_end_matches(['\n', '\r']).to_owned())
    }

    /// Standard output as UTF-8.
    pub(crate) fn text(&self) -> Result<&str> {
        std::str::from_utf8(&self.stdout)
            .map_err(|error| Error::Git(format!("git produced invalid UTF-8 output: {error}")))
    }

    /// Standard error as lossy UTF-8 for diagnostics.
    pub(crate) fn diagnostic(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }

    /// Whether the invocation exited successfully.
    pub(crate) fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

impl<'a> GitCommand<'a> {
    /// Start a `git` invocation inside `directory`.
    pub(crate) fn new(directory: &'a Path) -> Self {
        Self {
            directory,
            arguments: Vec::new(),
            environment: Vec::new(),
            stdin: None,
        }
    }

    /// Append one literal argument.
    pub(crate) fn arg(mut self, value: impl AsRef<str>) -> Self {
        self.arguments.push(value.as_ref().to_owned());
        self
    }

    /// Append several literal arguments.
    pub(crate) fn args<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for value in values {
            self.arguments.push(value.as_ref().to_owned());
        }
        self
    }

    /// Set one environment variable for this invocation.
    pub(crate) fn env(mut self, key: &str, value: impl AsRef<str>) -> Self {
        self.environment
            .push((key.to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Supply standard input bytes.
    pub(crate) fn stdin(mut self, value: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(value.into());
        self
    }

    /// Run the invocation and capture its result without asserting success.
    pub(crate) fn output(self) -> Result<GitOutput> {
        let mut command = Command::new("git");
        command
            .current_dir(self.directory)
            // Repository-local hooks, pager, and signature-display
            // configuration must never influence a release identity or a
            // verification decision, and must never prefix parsed output.
            .arg("-c")
            .arg("core.hooksPath=/dev/null")
            .arg("-c")
            .arg("log.showSignature=false")
            .args(&self.arguments)
            .env("GIT_PAGER", "cat")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &self.environment {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|error| {
            Error::Git(format!(
                "failed to run git {}: {error}",
                self.arguments.join(" ")
            ))
        })?;
        if let Some(bytes) = &self.stdin {
            let mut handle = child
                .stdin
                .take()
                .ok_or_else(|| Error::Git("git standard input was unavailable".to_owned()))?;
            handle
                .write_all(bytes)
                .map_err(|error| Error::Git(format!("failed to write git input: {error}")))?;
            drop(handle);
        }
        let output = child.wait_with_output().map_err(|error| {
            Error::Git(format!(
                "failed to collect git {} output: {error}",
                self.arguments.join(" ")
            ))
        })?;
        Ok(GitOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            code: output.status.code(),
        })
    }

    /// Run the invocation and require a successful exit status.
    pub(crate) fn run(self) -> Result<GitOutput> {
        let description = format!("git {}", self.arguments.join(" "));
        let output = self.output()?;
        if !output.succeeded() {
            return Err(Error::Git(format!(
                "{description} failed: {}",
                output.diagnostic()
            )));
        }
        Ok(output)
    }
}

/// Resolve one revision to its full object identity.
pub(crate) fn resolve(directory: &Path, revision: &str) -> Result<String> {
    GitCommand::new(directory)
        .args(["rev-parse", "--verify"])
        .arg(format!("{revision}^{{object}}"))
        .run()?
        .line()
}

/// Whether an object is present in the repository's object database.
pub(crate) fn has_object(directory: &Path, object: &str) -> Result<bool> {
    Ok(GitCommand::new(directory)
        .args(["cat-file", "-e"])
        .arg(format!("{object}^{{object}}"))
        .output()?
        .succeeded())
}
