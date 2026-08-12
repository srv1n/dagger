//! Parse one workflow YAML file through dagger-workflow-core's strict authority.

use dagger_workflow_core::definition::parse_yaml_definition;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: definition-lint <definition.yaml>")?;
    let source = std::fs::read_to_string(&path)?;
    let definition = parse_yaml_definition(&source)?;
    println!(
        "{{\"definition_id\":\"{}\",\"node_count\":{},\"status\":\"accepted\"}}",
        definition.definition_id.as_str(),
        definition.nodes.len()
    );
    Ok(())
}
