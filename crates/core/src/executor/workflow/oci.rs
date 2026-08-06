// ---
// relationships:
//   implements: github-release-executor
// ---

// Open Container Initiative build derivation moved from `executor::workflow`.

use super::*;

/// Values one packager's build command reads from its step environment.
///
/// Buildx annotates the index it seals. Cargo archive names the formula and
/// removes the global tag's literal affixes to obtain its version. The subject
/// name and tag affixes reach both bodies as data rather than as source.
pub(super) fn build_environment(subject: &DistinctSubject, global_tag: &str) -> String {
    if !matches!(subject.packager, Packager::Buildx | Packager::CargoArchive) {
        return String::new();
    }
    let (prefix, suffix) = global_tag
        .split_once("{version}")
        .unwrap_or((global_tag, ""));
    format!(
        "      @ENVVAR@SUBJECT_IDENTITY: {}\n      @ENVVAR@TAG_PREFIX: {}\n      @ENVVAR@TAG_SUFFIX: {}\n",
        scalar(&subject.identity),
        scalar(prefix),
        scalar(suffix),
    )
}
