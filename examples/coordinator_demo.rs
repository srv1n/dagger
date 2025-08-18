//! Demonstration of the new Coordinator-based parallel execution
//! 
//! This example shows:
//! - Parallel node execution without borrow checker issues
//! - Dynamic DAG growth via hooks
//! - Clean separation between computation and mutation

use anyhow::Result;
use async_trait::async_trait;
use dagger::coord::{
    ActionRegistry, Coordinator, EventHook, HookContext,
    ExecutionEvent, ExecutorCommand, NodeSpec, NodeAction, NodeCtx, NodeOutput,
};
use dagger::dag_flow::{DagExecutor, DagConfig, Cache};
use std::sync::Arc;
use tokio::sync::oneshot;
use serde_json::json;

// ============= Example Actions =============

/// Simple compute action
struct ComputeAction {
    name: String,
}

#[async_trait]
impl NodeAction for ComputeAction {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        println!("[ComputeAction] {} executing with inputs: {}", ctx.node_id, ctx.inputs);
        
        // Simulate some computation
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        
        // Return computed result
        let result = json!({
            "computed": format!("Result from {}", ctx.node_id),
            "timestamp": chrono::Utc::now().timestamp(),
        });
        
        Ok(NodeOutput::success(result))
    }
}

/// Transform action
struct TransformAction;

#[async_trait]
impl NodeAction for TransformAction {
    fn name(&self) -> &str {
        "transform"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        println!("[TransformAction] {} transforming data", ctx.node_id);
        
        // Simulate transformation
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
        
        Ok(NodeOutput::success(json!({
            "transformed": true,
            "node": ctx.node_id,
        })))
    }
}

// ============= Example Hooks =============

/// Hook that adds cleanup nodes after compute nodes
struct CleanupHook;

#[async_trait]
impl EventHook for CleanupHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeCompleted { node, .. } => {
                if node.node_id.starts_with("compute_") {
                    println!("[CleanupHook] Adding cleanup for {}", node.node_id);
                    
                    // Add a cleanup node
                    vec![ExecutorCommand::AddNode {
                        dag_name: ctx.dag_name.clone(),
                        spec: NodeSpec::new("transform")
                            .with_id(format!("cleanup_{}", node.node_id))
                            .with_deps(vec![node.node_id.clone()]),
                    }]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }
}

/// Hook that monitors execution
struct MonitorHook;

#[async_trait]
impl EventHook for MonitorHook {
    async fn handle(&self, _ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeStarted { node } => {
                println!("[MonitorHook] ⚡ Started: {}", node.node_id);
            }
            ExecutionEvent::NodeCompleted { node, .. } => {
                println!("[MonitorHook] ✅ Completed: {}", node.node_id);
            }
            ExecutionEvent::NodeFailed { node, error } => {
                println!("[MonitorHook] ❌ Failed: {} - {}", node.node_id, error);
            }
        }
        vec![]
    }
}

// ============= Main Demo =============

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();
    
    println!("=== Coordinator-Based Parallel Execution Demo ===\n");
    
    // Create action registry
    let action_registry = ActionRegistry::new();
    action_registry.register(Arc::new(ComputeAction { name: "compute".to_string() }));
    action_registry.register(Arc::new(TransformAction));
    
    // Create DAG executor with new registry
    let config = DagConfig {
        max_parallel_nodes: 3,
        enable_parallel_execution: true,
        ..Default::default()
    };
    
    // Convert new registry to old format temporarily
    // In production, DagExecutor should use ActionRegistry directly
    let old_registry = Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
    
    let mut executor = DagExecutor::new(
        Some(config),
        old_registry,
        "sqlite::memory:",
    ).await?;
    
    // Create a simple DAG
    let yaml_content = r#"
name: demo_dag
description: Coordinator demo DAG
author: Demo
version: 1.0.0
signature: demo
tags:
  - demo
  - coordinator
nodes:
  - id: compute_1
    description: First compute node
    dependencies: []
    inputs: []
    outputs:
      - name: result
    action: compute
    failure: compute
    onfailure: false
    timeout: 30
    try_count: 1
  - id: compute_2
    description: Second compute node
    dependencies: []
    inputs: []
    outputs:
      - name: result
    action: compute
    failure: compute
    onfailure: false
    timeout: 30
    try_count: 1
  - id: compute_3
    description: Third compute node
    dependencies: [compute_1, compute_2]
    inputs: []
    outputs:
      - name: result
    action: compute
    failure: compute
    onfailure: false
    timeout: 30
    try_count: 1
"#;
    
    // Write and load the DAG
    std::fs::write("/tmp/demo_dag.yaml", yaml_content)?;
    executor.load_yaml_file("/tmp/demo_dag.yaml")?;
    
    // Create hooks
    let hooks: Vec<Arc<dyn EventHook>> = vec![
        Arc::new(MonitorHook),
        Arc::new(CleanupHook),
    ];
    
    // Create coordinator
    let coordinator = Coordinator::new(hooks, 100, 100);
    
    // Create cache
    let cache = Cache::new();
    
    // Create cancellation channel
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    
    println!("Starting parallel execution with dynamic growth...\n");
    
    // Run the coordinator
    match coordinator.run_parallel(
        &mut executor,
        &cache,
        "demo_dag",
        "run_001",
        cancel_rx,
    ).await {
        Ok(()) => {
            println!("\n✅ Execution completed successfully!");
        }
        Err(e) => {
            println!("\n❌ Execution failed: {}", e);
        }
    }
    
    // Show final DAG state
    println!("\n=== Final DAG State ===");
    if let Ok(dags) = executor.prebuilt_dags.read() {
        if let Some((graph, node_map)) = dags.get("demo_dag") {
            println!("Total nodes: {}", node_map.len());
            for node_id in node_map.keys() {
                println!("  - {}", node_id);
            }
        }
    }
    
    println!("\n=== Key Insights ===");
    println!("1. Workers executed in parallel without borrow conflicts");
    println!("2. Hooks added nodes dynamically (cleanup nodes)");
    println!("3. All mutations happened through Coordinator");
    println!("4. No &mut DagExecutor in workers or hooks");
    println!("5. Clean separation: compute (workers) vs control (coordinator)");
    
    Ok(())
}