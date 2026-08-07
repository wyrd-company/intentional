// ---
// relationships:
//   tests: github-release-publication
// ---

use std::collections::BTreeSet;

fn feature_scenario_bindings(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            line.split_once("intentional-feature-scenario: ")
                .map(|(_, binding)| binding.trim().to_owned())
        })
        .collect()
}

fn executable_feature_scenario_bindings(source: &str) -> BTreeSet<String> {
    let lines = source.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let binding = line.split_once("intentional-feature-scenario: ")?.1.trim();
            let test = binding
                .rsplit_once('=')
                .map(|(_, test)| test)
                .expect("feature scenario binding names its test function");
            assert!(
                lines
                    .iter()
                    .skip(index + 1)
                    .take(4)
                    .any(|line| line.trim_start().starts_with(&format!("fn {test}()"))),
                "feature scenario binding {binding} annotates its named test function"
            );
            Some(binding.to_owned())
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
