use dagger::{Cache, DagExecutor, DagConfig, insert_value};
use dagger::dag_flow::ExecutionContext;
use dagger::coord::{ActionRegistry, NodeAction, NodeCtx, NodeOutput};
use async_trait::async_trait;
use std::sync::Arc;
use serde_json::json;
use tokio::sync::{oneshot, RwLock, Semaphore};
use tokio::time::Instant;

// Simple action that prints a message
struct PrintMessageAction;

#[async_trait]
impl NodeAction for PrintMessageAction {
    fn name(&self) -> &str {
        "print_message"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        println!("=== Executing node: {} ===", ctx.node_id);
        println!("  Inputs: {:?}", ctx.inputs);
        
        // Store output in cache for dependent nodes
        insert_value(&ctx.cache, &ctx.node_id, "output", json!({
            "message": format!("Processed by {}", ctx.node_id),
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }))?;
        
        // Simulate some work
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        
        println!("  Node {} completed", ctx.node_id);
        
        Ok(NodeOutput::success(json!({
            "status": "completed",
            "node": ctx.node_id.clone(),
        })))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    println!("\n=== Testing Fixed DAG Flow ===\n");
    
    // Create registry and register the print_message action
    let registry = ActionRegistry::new();
    registry.register(Arc::new(PrintMessageAction));
    
    // Create executor with parallel execution enabled
    let config = DagConfig {
        enable_parallel_execution: true,
        max_parallel_nodes: 4,
        ..Default::default()
    };
    
    let mut executor = DagExecutor::new(
        Some(config.clone()),
        registry.clone(),
        "sqlite::memory:"
    ).await?;
    
    // Set up execution context for parallel execution
    executor.execution_context = Some(ExecutionContext {
        max_parallel_nodes: config.max_parallel_nodes,
        semaphore: Arc::new(Semaphore::new(config.max_parallel_nodes)),
        cache_last_snapshot: Arc::new(RwLock::new(Instant::now())),
        cache_delta_size: Arc::new(RwLock::new(0)),
    });
    
    // Load the test DAG from YAML
    executor.load_yaml_file("test_sequential.yaml").await?;
    
    // Create cache and run the DAG
    let cache = Cache::new();
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    
    println!("Starting DAG execution...\n");
    let start_time = std::time::Instant::now();
    
    let report = executor.execute_static_dag("test_sequential", &cache, cancel_rx).await?;
    
    let elapsed = start_time.elapsed();
    
    println!("\n=== Execution Complete ===");
    println!("Success: {}", report.overall_success);
    println!("Nodes executed: {}", report.node_outcomes.len());
    println!("Time taken: {:?}", elapsed);
    
    if let Some(error) = report.error {
        println!("Error: {}", error);
    }
    
    // Print cache contents to verify data flow
    println!("\n=== Cache Contents ===");
    let cache_json = dagger::serialize_cache_to_prettyjson(&cache)?;
    println!("{}", cache_json);
    
    Ok(())
}