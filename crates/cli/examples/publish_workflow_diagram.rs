// ---
// relationships:
//   generates: publish-workflow
// ---

use intentional_core::config::WorkflowRole;
use intentional_core::executor::{
    compare_workflow,
    fixture::{derived_workflows, publish_workflow_kind_mermaid},
};
use std::io::Write;

const WORKFLOW_SEED: &str =
    "name: Publish\n\non: {}\n\npermissions:\n  contents: read\n\njobs: {}\n";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("docs/assets/publish-workflow.mmd"));
    let mut workflow = tempfile::Builder::new()
        .prefix("intentional-publish-diagram-")
        .suffix(".yml")
        .tempfile_in(&root)?;
    workflow.write_all(WORKFLOW_SEED.as_bytes())?;
    compare_workflow(&root, WorkflowRole::Publish, Some(workflow.path()))?.apply()?;
    let mut derivations = vec![std::fs::read_to_string(workflow.path())?];
    derivations.extend(
        derived_workflows("publish-docs-conditional-jobs")
            .into_iter()
            .filter_map(|(role, workflow)| (role == WorkflowRole::Publish).then_some(workflow)),
    );
    std::fs::write(output, publish_workflow_kind_mermaid(&derivations))?;
    Ok(())
}
