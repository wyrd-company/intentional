// ---
// relationships:
//   implements: github-release-executor
// ---

//! Release evidence contribution transport.

pub mod contribution;

use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
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

/// Whether a value is a canonical `sha256:<64 lowercase hex>` digest.
pub(crate) fn is_digest(value: &str) -> bool {
    value
        .strip_prefix(DIGEST_PREFIX)
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && value.to_ascii_lowercase() == value
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

/// Require that a directory is absent or empty before one writer fills it.
///
/// Evidence bundles are written exactly once, so refusing a populated
/// directory keeps a partially written or previously assembled bundle from
/// being silently merged with a new one.
pub(crate) fn prepare_output(path: &Path, label: &str) -> Result<()> {
    if path.exists() {
        if !path.is_dir() {
            return Err(Error::Validation(format!(
                "{label} output {} is not a directory",
                path.display()
            )));
        }
        let mut entries = std::fs::read_dir(path).map_err(|error| Error::io(path, error))?;
        if entries.next().is_some() {
            return Err(Error::Validation(format!(
                "{label} output directory {} is not empty; assembly writes one complete bundle",
                path.display()
            )));
        }
    }
    std::fs::create_dir_all(path).map_err(|error| Error::io(path, error))
}
