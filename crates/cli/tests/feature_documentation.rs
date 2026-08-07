// ---
// relationships:
//   tests: github-release-publication
// ---

use std::collections::BTreeSet;

fn feature_scenario_bindings(source: &str) -> BTreeSet<String> {
    let lines = source.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let scenario = line.split_once("intentional-feature-scenario: ")?.1.trim();
            let test = lines
                .get(index + 1)
                .and_then(|line| line.split_once("intentional-feature-test: "))
                .map(|(_, test)| test.trim())
                .unwrap_or_else(|| panic!("feature scenario {scenario} names its test"));
            Some(format!("{scenario}={test}"))
        })
        .collect()
}

fn executable_feature_scenario_bindings(source: &str) -> BTreeSet<String> {
    let lines = source.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let scenario = line.split_once("intentional-feature-scenario: ")?.1.trim();
            let test = lines
                .get(index + 1)
                .and_then(|line| line.split_once("intentional-feature-test: "))
                .map(|(_, test)| test.trim())
                .unwrap_or_else(|| panic!("feature scenario {scenario} names its test"));
            assert!(
                lines
                    .iter()
                    .skip(index + 2)
                    .take(3)
                    .any(|line| line.trim_start().starts_with(&format!("fn {test}()"))),
                "feature scenario {scenario} annotates its named test function {test}"
            );
            Some(format!("{scenario}={test}"))
        })
        .collect()
}

#[test]
fn feature_alias_scenario_is_bound_to_both_packager_recipes() {
    let feature = std::fs::read_to_string("../../docs/features/github-release-publication.yml")
        .expect("publication feature is readable");
    let recipes = std::fs::read_to_string("../core/src/executor/workflow/tests/oci_recipes.rs")
        .expect("OCI recipe tests are readable");
    let documented = feature_scenario_bindings(&feature);
    let executable = executable_feature_scenario_bindings(&recipes);

    assert_eq!(
        documented,
        executable,
        "every documented packager branch names one executable witness and every witness names its scenario"
    );
    assert_eq!(
        documented.len(),
        2,
        "the prerelease alias scenario covers runnable images and Dev Container Features"
    );
}
