//! Example demonstrating dynamic node addition during DAG execution
//! 
//! This example shows:
//! - Starting with a simple DAG
//! - Using hooks to dynamically add nodes based on results
//! - Proper use of the Coordinator with cache

use dagger::{
    Cache, DagExecutor, DagConfig, insert_value, get_input,
    dag_flow::{OnFailure, HumanTimeoutAction, RetryStrategy},
    coord::{
        Coordinator, ActionRegistry, NodeAction, NodeCtx, NodeOutput,
        EventHook, HookContext, ExecutionEvent, ExecutorCommand,
        types::{NodeSpec, NodeOutcome},
    },
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing_subscriber;

/// Simple action that adds two numbers
struct AddAction;

#[async_trait]
impl NodeAction for AddAction {
    fn name(&self) -> &str {
        "add"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        let a = ctx.inputs.get("a")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let b = ctx.inputs.get("b")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        
        let result = a + b;
        println!("[{}] Adding {} + {} = {}", ctx.node_id, a, b, result);
        
        // Store result in cache
        insert_value(&ctx.cache, &ctx.node_id, "result", json!(result))?;
        
        Ok(NodeOutput::success(json!({
            "result": result
        })))
    }
}

/// Action that multiplies a number by 2
struct MultiplyAction;

#[async_trait]
impl NodeAction for MultiplyAction {
    fn name(&self) -> &str {
        "multiply"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        let input = ctx.inputs.get("value")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        
        let result = input * 2.0;
        println!("[{}] Multiplying {} * 2 = {}", ctx.node_id, input, result);
        
        // Store result in cache
        insert_value(&ctx.cache, &ctx.node_id, "result", json!(result))?;
        
        Ok(NodeOutput::success(json!({
            "result": result
        })))
    }
}

/// Hook that dynamically adds nodes based on results
struct DynamicGrowthHook {
    threshold: f64,
    cache: Cache,
}

#[async_trait]
impl EventHook for DynamicGrowthHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        let mut commands = Vec::new();
        
        match event {
            ExecutionEvent::NodeCompleted { node, outcome } => {
                // Check if this was an "add" node
                if node.node_id.starts_with("add_") {
                    // Extract the result from the outcome
                    if let NodeOutcome::Success { outputs: Some(outputs) } = outcome {
                        if let Some(result) = outputs.get("result").and_then(|v| v.as_f64()) {
                            println!("Hook: Node {} completed with result {}", node.node_id, result);
                            
                            // If result exceeds threshold, add a multiply node
                            if result > self.threshold {
                                println!("Hook: Result {} exceeds threshold {}, adding multiply node", 
                                    result, self.threshold);
                                
                                let multiply_node_id = format!("multiply_{}", node.node_id);
                                
                                // Store the input value for the multiply node in cache
                                let _ = insert_value(&self.cache, &multiply_node_id, "value", json!(result));
                                
                                // Create a multiply node that depends on the add node
                                let spec = NodeSpec {
                                    id: Some(multiply_node_id.clone()),
                                    action: "multiply".to_string(),
                                    deps: vec![node.node_id.clone()],
                                    inputs: json!({
                                        "value": result
                                    }),
                                    timeout: Some(30),
                                    try_count: Some(1),
                                };
                                
                                commands.push(ExecutorCommand::AddNode {
                                    dag_name: ctx.dag_name.clone(),
                                    spec,
                                });
                                
                                println!("Hook: Scheduled multiply node: {}", multiply_node_id);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        
        commands
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    println!("\n=== Dynamic Node Addition Demo ===\n");
    
    // Create action registry
    let mut registry = ActionRegistry::new();
    registry.register(Arc::new(AddAction));
    registry.register(Arc::new(MultiplyAction));
    
    // Create executor with config
    let config = DagConfig {
        max_attempts: Some(1),
        on_failure: OnFailure::Stop,
        timeout_seconds: Some(60),
        human_wait_minutes: None,
        human_timeout_action: HumanTimeoutAction::Autopilot,
        max_tokens: None,
        max_iterations: None,
        review_frequency: None,
        retry_strategy: RetryStrategy::default(),
        enable_parallel_execution: true,
        max_parallel_nodes: 2,
        enable_incremental_cache: false,
        cache_snapshot_interval: 30,
    };
    
    let mut executor = DagExecutor::new(
        Some(config),
        registry,
        "sqlite::memory:",
    ).await?;
    
    // Load the minimal DAG from YAML
    executor.load_yaml_file("examples/dynamic_dag.yaml").await?;
    let dag_name = "dynamic_dag";
    
    // Create cache first
    let cache = Cache::new();
    
    // Add initial nodes
    executor.add_node(dag_name, "add_1".to_string(), "add".to_string(), vec![]).await?;
    executor.add_node(dag_name, "add_2".to_string(), "add".to_string(), vec!["add_1".to_string()]).await?;
    
    // Set node inputs (this creates IFields and stores values)
    executor.set_node_inputs_json(dag_name, "add_1", json!({
        "a": 5.0,
        "b": 3.0
    }), &cache).await?;
    
    executor.set_node_inputs_json(dag_name, "add_2", json!({
        "a": 10.0,
        "b": 7.0
    }), &cache).await?;
    
    // Create the hook with threshold
    let hook = Arc::new(DynamicGrowthHook { 
        threshold: 10.0,
        cache: cache.clone(),
    });
    
    // Create coordinator with the hook
    let coordinator = Coordinator::new(
        vec![hook],
        100,  // event channel capacity
        100,  // command channel capacity
    );
    
    // Create cancellation channel
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    
    println!("Starting execution with initial nodes: add_1, add_2");
    println!("Hook will add multiply nodes for results > 10.0\n");
    
    // Run the DAG with coordinator
    let start = std::time::Instant::now();
    coordinator.run_parallel(
        &mut executor,
        &cache,
        dag_name,
        "run_001",
        cancel_rx,
    ).await?;
    
    println!("\n=== Execution Complete ===");
    println!("Time taken: {:?}", start.elapsed());
    
    // Print final cache state
    println!("\n=== Final Results ===");
    if let Ok(result) = get_input::<Value>(&cache, "add_1", "result") {
        println!("add_1 result: {}", result);
    }
    if let Ok(result) = get_input::<Value>(&cache, "add_2", "result") {
        println!("add_2 result: {}", result);
    }
    if let Ok(result) = get_input::<Value>(&cache, "multiply_add_1", "result") {
        println!("multiply_add_1 result: {}", result);
    }
    if let Ok(result) = get_input::<Value>(&cache, "multiply_add_2", "result") {
        println!("multiply_add_2 result: {}", result);
    }
    
    Ok(())
}