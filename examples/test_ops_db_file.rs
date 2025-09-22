use dagger::{DagExecutor, DagConfig, Cache, insert_value, ExecutionObserver};
use dagger::coord::registry::ActionRegistry;
use async_trait::async_trait;
use std::sync::Arc;
use serde_json::json;

// Simple observer implementation for testing
struct TestObserver;

#[async_trait]
impl ExecutionObserver for TestObserver {
    async fn on_node_started(&self, run_id: &str, dag_name: &str, node_id: &str) {
        println!("Node started: run={}, dag={}, node={}", run_id, dag_name, node_id);
    }
    
    async fn on_node_completed(&self, run_id: &str, dag_name: &str, node_id: &str) {
        println!("Node completed: run={}, dag={}, node={}", run_id, dag_name, node_id);
    }
    
    async fn on_node_failed(&self, run_id: &str, dag_name: &str, node_id: &str, error: &str) {
        println!("Node failed: run={}, dag={}, node={}, error={}", run_id, dag_name, node_id, error);
    }
    
    async fn on_nodes_added(&self, run_id: &str, dag_name: &str, node_ids: &[String]) {
        println!("Nodes added: run={}, dag={}, nodes={:?}", run_id, dag_name, node_ids);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Testing DagExecutor with file-based sqlite:ops.db");
    
    // Clean up any existing database files
    std::fs::remove_file("ops.db").ok();
    std::fs::remove_file("ops.db-shm").ok();
    std::fs::remove_file("ops.db-wal").ok();
    
    // Create executor with ops.db file
    let registry = ActionRegistry::new();
    let config = DagConfig::default();
    // Using file-based database with proper format
    let mut executor = DagExecutor::new(Some(config), registry, "sqlite:ops.db").await?;
    
    println!("Successfully created DagExecutor with sqlite:ops.db");
    
    // Add an observer
    let observer = Arc::new(TestObserver);
    executor.add_observer(observer);
    
    // Test cache accessors
    let cache = Cache::new();
    
    // Set a value using the new API
    executor.set_node_output_json("test_dag", "llm_node", "llm_out", &json!({
        "role": "assistant",
        "content": "Hello from file-based DB!",
        "turn": 1
    })).await?;
    
    // Get the value back
    let result = executor.get_node_output_json("test_dag", "llm_node", "llm_out").await?;
    println!("Retrieved value: {:?}", result);
    
    // Verify database file was created
    if std::path::Path::new("ops.db").exists() {
        println!("✓ ops.db file was created successfully");
    } else {
        println!("✗ ops.db file was NOT created");
    }
    
    // Clean up
    std::fs::remove_file("ops.db").ok();
    std::fs::remove_file("ops.db-shm").ok();
    std::fs::remove_file("ops.db-wal").ok();
    
    println!("Test completed successfully!");
    Ok(())
}