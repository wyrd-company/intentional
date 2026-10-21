// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Public types in `intentional-core` that are open on purpose.
//!
//! Each entry is a module path inside the crate and a one-line reason. See
//! `README.md` beside this file before adding one.

/// Module path and reason for each intentionally open public type.
pub const OPEN_TYPES: &[(&str, &str)] = &[
    (
        "intent::IntentDraft",
        "an input callers build; a new member must be supplied, so closing does not make it additive",
    ),
    ("model::Bump", "vocabulary enum mirroring a published schema enum"),
    ("model::ProjectionMode", "vocabulary enum mirroring a published schema enum"),
    ("model::Pre1BumpMapping", "vocabulary enum mirroring a published schema enum"),
    ("model::ReleaseUnitDisposition", "vocabulary enum mirroring a published schema enum"),
    (
        "model::ReleaseRecordPolicy",
        "vocabulary enum mirroring a published schema enum; a closed set",
    ),
    ("model::TagRole", "vocabulary enum mirroring a published schema enum"),
    ("model::TagPhase", "vocabulary enum mirroring a published schema enum"),
    ("tag_record::TagRecordRole", "vocabulary enum mirroring a published schema enum"),
    ("verify::Severity", "vocabulary enum mirroring a published schema enum"),
    ("init::InitState", "vocabulary enum mirroring a published schema enum"),
    (
        "init::CandidateResolution",
        "vocabulary enum mirroring a published schema enum; tests build it as an init answer",
    ),
    ("version::VersionRepository", "fields are private; not a record or result"),
    ("adapters::ecosystem::NpmAdapter", "unit adapter struct; not a record or result"),
    ("adapters::ecosystem::CargoAdapter", "unit adapter struct; not a record or result"),
    ("adapters::ecosystem::PubAdapter", "unit adapter struct; not a record or result"),
    ("adapters::ecosystem::PythonAdapter", "unit adapter struct; not a record or result"),
    ("adapters::ecosystem::MsbuildAdapter", "unit adapter struct; not a record or result"),
    ("adapters::ecosystem::GoAdapter", "unit adapter struct; not a record or result"),
    ("adapters::format::JsonFormat", "unit format struct; not a record or result"),
    ("adapters::format::TomlFormat", "unit format struct; not a record or result"),
    ("adapters::format::YamlFormat", "unit format struct; not a record or result"),
];
