// ---
// relationships:
//   tests: github-release-executor
// ---

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

    assert!(
        page.contains(&format!("at most {limit} lines")),
        "the adopter page states the implementation's workflow readback limit"
    );
    assert!(
        page.contains("Move unrelated repository jobs to another workflow"),
        "the adopter page gives a remedy for repository-owned workflow size"
    );
    assert!(
        page.contains("reduce configured publication destinations"),
        "the adopter page gives a remedy for managed publication size"
    );
}
