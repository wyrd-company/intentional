// ---
// relationships:
//   implements: github-release-executor
// ---

//! Release evidence contribution transport and deterministic final assembly.

pub mod assemble;
pub mod contribution;
pub mod identity;
pub mod phase;

use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::Path;

/// Prefix every SHA-256 digest recorded by an evidence document carries.
pub const DIGEST_PREFIX: &str = "sha256:";

/// Digest one byte sequence in the canonical `sha256:<hex>` form.
pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    format!("{DIGEST_PREFIX}{:x}", Sha256::digest(bytes))
}

/// Copy one file into `destination` while digesting exactly the bytes copied.
///
/// Reading the source once keeps the recorded digest bound to the transported
/// bytes, so a file that changes during the copy cannot be inventoried under a
/// digest it never had.
pub(crate) fn copy_and_digest(source: &Path, destination: &Path) -> Result<String> {
    let mut reader = std::fs::File::open(source).map_err(|error| Error::io(source, error))?;
    let mut writer =
        std::fs::File::create(destination).map_err(|error| Error::io(destination, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| Error::io(source, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|error| Error::io(destination, error))?;
    }
    writer
        .flush()
        .map_err(|error| Error::io(destination, error))?;
    Ok(format!("{DIGEST_PREFIX}{:x}", hasher.finalize()))
}

/// Digest one existing file without loading it entirely into memory.
pub(crate) fn digest_file(path: &Path) -> Result<String> {
    let mut reader = std::fs::File::open(path).map_err(|error| Error::io(path, error))?;
    let mut hasher = Sha256::new();
    io::copy(&mut reader, &mut hasher).map_err(|error| Error::io(path, error))?;
    Ok(format!("{DIGEST_PREFIX}{:x}", hasher.finalize()))
}

/// Whether a value is a canonical `sha256:<64 lowercase hex>` digest.
pub(crate) fn is_digest(value: &str) -> bool {
    value
        .strip_prefix(DIGEST_PREFIX)
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && value.to_ascii_lowercase() == value
}

/// Whether a value is a full SHA-1 or SHA-256 Git object identifier.
pub(crate) fn is_git_object(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Whether a contributor namespace can be used as an evidence key.
///
/// Namespaces are contributor-owned identity, so the only constraints are the
/// mechanical ones that keep the assembled document readable and unambiguous.
pub(crate) fn is_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && value.len() <= 200
}

/// Whether a name is usable as a flat GitHub Release asset name.
pub(crate) fn is_flat_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
}

/// Build one write-once bundle and move it into place only once it is complete.
///
/// Evidence bundles are written exactly once, so refusing a populated output
/// keeps a previously written bundle from being silently merged with a new one.
/// Building in a sibling staging directory keeps that rule from turning a
/// transient fault into a permanently unwritable output: a failed run leaves
/// nothing behind, so the next run — a workflow retry included — starts from
/// the same state as the first.
///
/// A directory that content raced into after the emptiness check is refused
/// rather than overwritten, on every platform.
pub(crate) fn write_bundle<T>(
    output: &Path,
    label: &str,
    build: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    if output.exists() {
        if !output.is_dir() {
            return Err(Error::Validation(format!(
                "{label} output {} is not a directory",
                output.display()
            )));
        }
        let mut entries = std::fs::read_dir(output).map_err(|error| Error::io(output, error))?;
        if entries.next().is_some() {
            return Err(Error::Validation(format!(
                "{label} output directory {} is not empty; one complete bundle is written once",
                output.display()
            )));
        }
    }
    let parent = output.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    let staging = parent.join(format!(
        ".intentional-{label}-staging-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| Error::io(&staging, error))?;
    let built = build(&staging).and_then(|built| {
        // Renaming the complete directory into place is the single step that
        // makes the bundle observable. A verified-empty output is removed
        // first, because replacing an existing directory by rename is a Unix
        // guarantee that Windows does not share. `remove_dir` refuses a
        // directory that gained content after the emptiness check, so the
        // window between the two stays fail-closed.
        if output.exists() {
            std::fs::remove_dir(output).map_err(|error| Error::io(output, error))?;
        }
        std::fs::rename(&staging, output).map_err(|error| Error::io(output, error))?;
        Ok(built)
    });
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    built
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::executor::fixture::Workspace;

    #[test]
    fn a_failed_build_leaves_no_output_and_no_staging_residue() {
        let workspace = Workspace::new("bundle-failure");
        let output = workspace.root().join("bundle");
        let error = write_bundle(&output, "evidence", |staging| {
            std::fs::write(staging.join("partial.txt"), "partial")
                .map_err(|error| Error::io(staging, error))?;
            Err::<(), _>(Error::Validation("build failed".to_owned()))
        })
        .expect_err("the failure is reported");
        assert!(error.to_string().contains("build failed"), "{error}");
        assert!(!output.exists(), "a failed build publishes nothing");
        assert_eq!(
            std::fs::read_dir(workspace.root())
                .expect("workspace")
                .count(),
            0,
            "a failed build leaves no staging directory behind"
        );

        // A transient failure must not make the output permanently unwritable.
        write_bundle(&output, "evidence", |staging| {
            std::fs::write(staging.join("complete.txt"), "complete")
                .map_err(|error| Error::io(staging, error))
        })
        .expect("the next attempt succeeds");
        assert!(output.join("complete.txt").is_file());
    }

    #[test]
    fn refuses_an_output_directory_that_gained_content_after_the_check() {
        let workspace = Workspace::new("bundle-race");
        let output = workspace.root().join("bundle");
        std::fs::create_dir_all(&output).expect("empty output");
        let error = write_bundle(&output, "evidence", |staging| {
            std::fs::write(staging.join("statement.yml"), "written")
                .map_err(|error| Error::io(staging, error))?;
            // Content races into the verified-empty output before the move.
            std::fs::write(output.join("raced.txt"), "raced")
                .map_err(|error| Error::io(&output, error))
        })
        .expect_err("the raced output is refused");
        assert!(
            error.to_string().contains(&output.display().to_string()),
            "{error}"
        );
        assert!(
            output.join("raced.txt").is_file(),
            "a refused move leaves the output as it found it"
        );
        assert!(
            !output.join("statement.yml").exists(),
            "a refused move publishes nothing"
        );
    }

    #[test]
    fn an_empty_output_directory_is_filled_and_a_populated_one_is_refused() {
        let workspace = Workspace::new("bundle-output");
        let output = workspace.root().join("bundle");
        std::fs::create_dir_all(&output).expect("empty output");
        write_bundle(&output, "evidence", |staging| {
            std::fs::write(staging.join("statement.yml"), "written")
                .map_err(|error| Error::io(staging, error))
        })
        .expect("an empty output directory accepts the bundle");
        assert_eq!(
            std::fs::read_to_string(output.join("statement.yml")).expect("statement"),
            "written"
        );

        let error = write_bundle(&output, "evidence", |staging| {
            std::fs::write(staging.join("statement.yml"), "replacement")
                .map_err(|error| Error::io(staging, error))
        })
        .expect_err("a populated output directory is refused");
        assert!(error.to_string().contains("is not empty"), "{error}");
        assert_eq!(
            std::fs::read_to_string(output.join("statement.yml")).expect("statement"),
            "written",
            "a refused write never replaces a complete bundle"
        );
    }
}
