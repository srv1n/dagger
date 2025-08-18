//! Test example for new DAG Flow enhancements
//! 
//! This example demonstrates the new features added to Dagger:
//! - Atomic node operations
//! - Supervisor hooks
//! - Event sink
//! - Branch registry
//! - Planning utilities

use anyhow::Result;
use dagger::dag_flow::{
    DagExecutor, DagConfig, Cache, Node, insert_value,
    SupervisorHook, LoggingSupervisor, 
    BufferingEventSink,
    NodeSpec, Plan,
};
use dagger::{NodeAction, register_action};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use async_trait::async_trait;

/// Custom supervisor that adds nodes dynamically
struct DynamicSupervisor;

#[async_trait]
impl SupervisorHook for DynamicSupervisor {
    async fn on_node_complete(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        _cache: &Cache,
    ) -> Result<()> {
        println!("DynamicSupervisor: Node {} completed", node.id);
        
        // Example: Add a cleanup node after certain nodes
        if node.id.starts_with("process_") {
            let cleanup_id = executor.gen_node_id("cleanup");
            println!("Adding cleanup node: {}", cleanup_id);
            // Note: In a real implementation, you'd add the node to the DAG
        }
        
        Ok(())
    }
}

/// Test action that just prints and succeeds
async fn test_action(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    println!("Executing test action for node: {}", node.id);
    insert_value(cache, &node.id, "result", format!("Completed {}", node.id))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("Testing DAG Flow Enhancements\n");
    
    // 1. Create executor with config
    let config = DagConfig {
        enable_parallel_execution: true,
        max_parallel_nodes: 2,
        ..Default::default()
    };
    
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(
        Some(config),
        registry,
        "sqlite::memory:"
    ).await?;
    
    // 2. Add supervisor hooks
    println!("=== Testing Supervisor Hooks ===");
    executor.add_supervisor_hook(Arc::new(LoggingSupervisor));
    executor.add_supervisor_hook(Arc::new(DynamicSupervisor));
    
    // 3. Set up event sink
    println!("\n=== Testing Event Sink ===");
    let event_sink = Arc::new(BufferingEventSink::new());
    executor.set_event_sink(event_sink.clone());
    
    // 4. Test branch registry
    println!("\n=== Testing Branch Registry ===");
    let branch_id = "test_branch";
    executor.branches.register_branch(branch_id);
    println!("Branch registered: {}", branch_id);
    
    executor.pause_branch(branch_id, Some("Testing pause"));
    println!("Branch paused");
    
    executor.resume_branch(branch_id);
    println!("Branch resumed");
    
    // 5. Test atomic node operations
    println!("\n=== Testing Atomic Node Operations ===");
    
    // Create a simple DAG by writing to a temp file
    let yaml_content = r#"
name: test_dag
description: Test DAG for enhancements
author: Test Author
version: 1.0.0
signature: test_signature
tags:
  - test
  - enhancement
nodes:
  - id: start
    description: Start node for testing
    dependencies: []
    inputs: []
    outputs:
      - name: result
    action: test_action
    failure: test_action
    onfailure: false
    timeout: 30
    try_count: 1
"#;
    
    // Register the test action BEFORE loading the YAML
    register_action!(executor, "test_action", test_action);
    
    // Write to a temp file and load it
    std::fs::write("/tmp/test_dag.yaml", yaml_content)?;
    executor.load_yaml_file("/tmp/test_dag.yaml")?;
    
    // Use atomic node addition
    let specs = vec![
        NodeSpec::new("test_action")
            .with_id("node1")
            .with_deps(vec!["start".to_string()])
            .with_timeout(30),
        NodeSpec::new("test_action")
            .with_id("node2")
            .with_deps(vec!["node1".to_string()])
            .with_timeout(30),
    ];
    
    let node_ids = executor.add_nodes_atomic("test_dag", specs)?;
    println!("Added nodes atomically: {:?}", node_ids);
    
    // 6. Test planning utilities
    println!("\n=== Testing Planning Utilities ===");
    let mut plan = Plan::new();
    
    let tool_id = plan.add_tool(
        "start",
        "example_tool",
        serde_json::json!({"param": "value"}),
        "run_123"
    );
    println!("Added tool node: {}", tool_id);
    
    let cont_id = plan.add_continuation(
        &[tool_id.clone()],
        serde_json::json!({"status": "processing"})
    );
    println!("Added continuation node: {}", cont_id);
    
    let llm_id = plan.add_next_llm(
        &cont_id,
        serde_json::json!({"model": "test"})
    );
    println!("Added LLM node: {}", llm_id);
    
    println!("Plan has {} nodes", plan.len());
    
    // 7. Test helper methods
    println!("\n=== Testing Helper Methods ===");
    
    // Generate unique node IDs
    let id1 = executor.gen_node_id("test");
    let id2 = executor.gen_node_id("test");
    println!("Generated IDs: {}, {}", id1, id2);
    assert_ne!(id1, id2, "IDs should be unique");
    
    // Check node existence
    let exists = executor.node_exists("test_dag", "start");
    println!("Node 'start' exists: {}", exists);
    
    // Compute levels for visualization
    match executor.compute_levels("test_dag") {
        Ok(levels) => {
            println!("\nDAG Levels:");
            for (level, nodes) in levels {
                println!("  Level {}: {:?}", level, nodes);
            }
        }
        Err(e) => println!("Error computing levels: {}", e),
    }
    
    // 8. Check events that were emitted
    println!("\n=== Checking Emitted Events ===");
    let events = event_sink.get_events();
    println!("Total events emitted: {}", events.len());
    for (i, event) in events.iter().take(5).enumerate() {
        println!("  Event {}: {:?}", i + 1, event.event);
    }
    
    println!("\n✅ All enhancement tests completed successfully!");
    
    Ok(())
}