// ---
// relationships:
//   tests: github-release-executor
// ---

fn normalized(source: &str) -> String {
    source
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn workflow_size_remedies(source: &str) -> Vec<&str> {
    let function = source
        .split_once("fn workflow_size_remedy(")
        .map(|(_, function)| function)
        .and_then(|function| function.split_once("\nfn oversized_workflow_line"))
        .map(|(function, _)| function)
        .expect("workflow implementation declares its size remedies");

    function
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix('"')
                .and_then(|line| line.strip_suffix('"'))
        })
        .map(|message| {
            message
                .split_once("; ")
                .map(|(_, remedy)| remedy)
                .expect("each workflow size diagnostic names its remedy")
        })
        .collect()
}

#[test]
fn publish_workflow_page_states_the_readback_bound_and_both_remedies() {
    let page = std::fs::read_to_string("../../docs/publish-workflow.md")
        .expect("publish workflow page is readable");
    let workflow = std::fs::read_to_string("../core/src/executor/workflow.rs")
        .expect("workflow implementation is readable");
    let limit = workflow
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("const MAX_WORKFLOW_LINES: usize = ")
        })
        .and_then(|value| value.strip_suffix(';'))
        .expect("workflow implementation declares its line limit")
        .replace('_', ",");
    let remedies = workflow_size_remedies(&workflow);
    let normalized_page = normalized(&page);

    assert!(
        page.contains(&format!("at most {limit} lines")),
        "the adopter page states the implementation's workflow readback limit"
    );
    assert_eq!(
        remedies.len(),
        2,
        "the workflow implementation declares release and publication remedies"
    );
    for remedy in remedies {
        assert!(
            normalized_page.contains(&normalized(remedy)),
            "the adopter page states the implementation remedy: {remedy}"
        );
    }
}
