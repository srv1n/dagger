use anyhow::Result;
use dagger::{
    DagExecutor, DagConfig, Cache, Node,
    register_action, insert_value, parse_input_from_name,
};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use tokio::sync::oneshot;

// Define custom actions
async fn fetch_data(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Simulate data fetching
    let source: String = parse_input_from_name(
        cache, "source", &node.inputs
    )?;
    
    let data = format!("Data from {}", source);
    insert_value(cache, &node.id, "data", data)?;
    
    Ok(())
}

async fn process_data(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    let input: String = parse_input_from_name(
        cache, "data", &node.inputs
    )?;
    
    let processed = input.to_uppercase();
    insert_value(cache, &node.id, "result", processed)?;
    
    Ok(())
}

async fn save_results(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    let data: String = parse_input_from_name(
        cache, "data", &node.inputs
    )?;
    
    println!("Saving: {}", data);
    insert_value(cache, &node.id, "status", "saved")?;
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize system
    let config = DagConfig {
        enable_parallel_execution: true,
        max_parallel_nodes: 2,
        ..Default::default()
    };
    
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(
        Some(config),
        registry,
        "sqlite::memory:"  // Use in-memory for testing
    ).await?;
    
    // Register actions
    register_action!(executor, "fetch_data", fetch_data);
    register_action!(executor, "process_data", process_data);
    register_action!(executor, "save_results", save_results);
    
    // Create a simple workflow programmatically
    use dagger::{GraphDefinition, Node as DagNode, IField, OField};
    
    let workflow = GraphDefinition {
        name: "test_workflow".to_string(),
        description: "Test workflow from documentation".to_string(),
        author: "Test".to_string(),
        version: "1.0.0".to_string(),
        nodes: vec![
            DagNode {
                id: "fetch".to_string(),
                dependencies: vec![],
                inputs: vec![
                    IField {
                        name: "source".to_string(),
                        description: Some("Data source".to_string()),
                        reference: "inputs.source".to_string(),
                    }
                ],
                outputs: vec![
                    OField {
                        name: "data".to_string(),
                        description: Some("Fetched data".to_string()),
                    }
                ],
                action: "fetch_data".to_string(),
                failure: "".to_string(),
                onfailure: false,
                description: "Fetch data".to_string(),
                timeout: 30,
                try_count: 2,
                instructions: None,
            },
            DagNode {
                id: "process".to_string(),
                dependencies: vec!["fetch".to_string()],
                inputs: vec![
                    IField {
                        name: "data".to_string(),
                        description: Some("Input data".to_string()),
                        reference: "fetch.data".to_string(),
                    }
                ],
                outputs: vec![
                    OField {
                        name: "result".to_string(),
                        description: Some("Processed result".to_string()),
                    }
                ],
                action: "process_data".to_string(),
                failure: "".to_string(),
                onfailure: false,
                description: "Process data".to_string(),
                timeout: 60,
                try_count: 3,
                instructions: None,
            },
            DagNode {
                id: "save".to_string(),
                dependencies: vec!["process".to_string()],
                inputs: vec![
                    IField {
                        name: "data".to_string(),
                        description: Some("Data to save".to_string()),
                        reference: "process.result".to_string(),
                    }
                ],
                outputs: vec![
                    OField {
                        name: "status".to_string(),
                        description: Some("Save status".to_string()),
                    }
                ],
                action: "save_results".to_string(),
                failure: "".to_string(),
                onfailure: false,
                description: "Save results".to_string(),
                timeout: 30,
                try_count: 3,
                instructions: None,
            },
        ],
        signature: "test_sig".to_string(),
        tags: vec!["test".to_string()],
    };
    
    // Register the workflow
    executor.dags.insert("test_workflow".to_string(), workflow);
    
    // Prepare inputs
    let cache = Cache::new();
    insert_value(&cache, "inputs", "source", "database")?;
    
    // Execute
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let report = executor.execute_static_dag(
        "test_workflow",
        &cache,
        cancel_rx
    ).await?;
    
    // Check results
    println!("Execution Report:");
    println!("  Total nodes: {}", report.total_nodes);
    println!("  Completed: {}", report.completed_nodes);
    println!("  Failed: {}", report.failed_nodes);
    println!("  Execution time: {:?}", report.execution_time);
    
    if report.failed_nodes == 0 {
        println!("\nSUCCESS: All documentation examples are working correctly!");
    } else {
        println!("\nFAILURE: Some nodes failed during execution");
    }
    
    Ok(())
}