// ---
// relationships:
//   tests: github-release-executor
// ---

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn action_outputs() -> BTreeMap<String, BTreeSet<String>> {
    let actions = repository_root().join("actions");
    std::fs::read_dir(&actions)
        .expect("Actions directory is readable")
        .filter_map(|entry| {
            let entry = entry.expect("Action directory entry is readable");
            let action = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path().join("action.yml");
            if !path.is_file() {
                return None;
            }
            let document: serde_yaml::Value = serde_yaml::from_str(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|_| panic!("{} is readable", path.display())),
            )
            .unwrap_or_else(|_| panic!("{} parses", path.display()));
            let outputs = document["outputs"]
                .as_mapping()
                .map(|outputs| {
                    outputs
                        .keys()
                        .map(|key| key.as_str().expect("Action output name is text").to_owned())
                        .collect()
                })
                .unwrap_or_default();
            Some((action, outputs))
        })
        .collect()
}

fn code_spans(line: &str) -> Vec<&str> {
    line.split('`')
        .enumerate()
        .filter_map(|(index, part)| (index % 2 == 1).then_some(part))
        .collect()
}

fn technical_design_outputs() -> BTreeMap<String, BTreeSet<String>> {
    let design = std::fs::read_to_string(
        repository_root().join("docs/technical-designs/github-release-executor.yml"),
    )
    .expect("technical design is readable");
    let contract = design
        .split_once("Their exact output\n    contracts are:")
        .expect("technical design introduces the exact Action output contracts")
        .1
        .split_once("The seal-phase-tags Action never pushes")
        .expect("technical design ends the Action output contracts")
        .0;

    let mut documented = BTreeMap::new();
    let mut action = None;
    let mut outputs = BTreeSet::new();
    for line in contract.lines() {
        let spans = code_spans(line);
        if line.trim_start().starts_with("- `") {
            if let Some(action) = action.replace(
                spans
                    .first()
                    .expect("output contract names an Action")
                    .to_string(),
            ) {
                documented.insert(action, std::mem::take(&mut outputs));
            }
            outputs.extend(spans.iter().skip(1).map(|output| (*output).to_owned()));
        } else if action.is_some() {
            outputs.extend(spans.into_iter().map(str::to_owned));
        }
    }
    if let Some(action) = action {
        documented.insert(action, outputs);
    }
    documented
}

fn normalized(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn command_line_specification_outputs(action: &str, specification: &str) -> BTreeSet<String> {
    let introduction = format!("The credential-free {action} Action");
    let outputs = specification
        .split_once(&introduction)
        .unwrap_or_else(|| panic!("command-line specification documents {action} outputs"))
        .1
        .split_once("outputs are exactly ")
        .unwrap_or_else(|| panic!("{action} documentation enumerates its outputs"))
        .1
        .split_once(", projected")
        .unwrap_or_else(|| panic!("{action} output enumeration ends before its projection rule"))
        .0
        .replace(", and ", ", ");
    outputs
        .split(", ")
        .map(str::trim)
        .map(str::to_owned)
        .collect()
}

#[test]
fn technical_design_action_outputs_match_every_shipped_action_in_both_directions() {
    assert_eq!(
        technical_design_outputs(),
        action_outputs(),
        "every shipped Action and only shipped Actions have their exact output set documented"
    );
}

#[test]
fn command_line_action_output_claims_match_their_shipped_actions() {
    let specification = normalized(
        &std::fs::read_to_string(
            repository_root().join("docs/specifications/command-line-interface.yml"),
        )
        .expect("command-line specification is readable"),
    );
    let manifests = action_outputs();
    let documented_actions = ["prepare-release", "verify-handoff", "verify-release-tag"];

    assert_eq!(
        specification.matches("outputs are exactly").count(),
        documented_actions.len(),
        "every exact output claim is assigned to a checked Action"
    );
    for action in documented_actions {
        assert_eq!(
            command_line_specification_outputs(action, &specification),
            manifests[action],
            "{action} documentation and manifest name the same outputs"
        );
    }
}
