// ---
// relationships:
//   tests: github-release-executor
// ---

use intentional_core::executor::fixture::configured_target_identities;
use std::collections::BTreeSet;

#[test]
fn credential_table_covers_every_configured_destination() {
    let usage = std::fs::read_to_string("../../docs/usage.md").expect("usage guide is readable");
    let section = usage
        .split_once("## Publish to a registry for the first time")
        .expect("usage guide has the registry section")
        .1;
    let table = section
        .split_once("| Publisher | Destination | Credential | Prefixed |")
        .expect("registry section has the credential table")
        .1;
    let rows = table
        .lines()
        .skip_while(|line| !line.starts_with("| ---"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            let publisher = line
                .split('|')
                .nth(1)
                .expect("credential row has a publisher")
                .trim()
                .trim_matches('`')
                .trim_end_matches(':')
                .to_owned();
            let target = line
                .split_once("<!-- intentional-target: ")
                .and_then(|(_, tail)| tail.split_once(" -->"))
                .map(|(target, _)| target.to_owned())
                .unwrap_or_else(|| panic!("credential row has a target identity: {line}"));
            (publisher, target)
        })
        .collect::<BTreeSet<_>>();
    let expected = configured_target_identities()
        .into_iter()
        .map(|(publisher, target)| (publisher.to_string(), target))
        .collect::<BTreeSet<_>>();

    assert_eq!(rows, expected, "credential rows match configured targets");
    assert!(
        section.contains(&format!("for {} in total.", expected.len())),
        "credential destination count is derived from configured targets"
    );
}
