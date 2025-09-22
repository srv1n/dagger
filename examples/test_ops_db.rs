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
    println!("Testing DagExecutor with sqlite:ops.db");
    
    // Create executor with ops.db - using memory for test
    let registry = ActionRegistry::new();
    let config = DagConfig::default();
    // Using memory database for testing - in production use "sqlite:ops.db"
    let mut executor = DagExecutor::new(Some(config), registry, "sqlite::memory:").await?;
    
    // Add an observer
    let observer = Arc::new(TestObserver);
    executor.add_observer(observer);
    
    // Test cache accessors
    let cache = Cache::new();
    
    // Set a value using the normal cache API
    insert_value(&cache, "test_node", "output", json!({"result": 42}))?;
    
    // Set a value using the new API
    executor.set_node_output_json("test_dag", "llm_node", "llm_out", &json!({
        "role": "assistant",
        "content": "Hello, world!",
        "turn": 1
    })).await?;
    
    // Get the value back
    let result = executor.get_node_output_json("test_dag", "llm_node", "llm_out").await?;
    println!("Retrieved value: {:?}", result);
    
    // Test with nested path
    executor.set_node_output_json("test_dag", "tool_node", "tools/response", &json!({
        "tool": "search",
        "results": ["item1", "item2"]
    })).await?;
    
    let tool_result = executor.get_node_output_json("test_dag", "tool_node", "tools/response").await?;
    println!("Retrieved tool result: {:?}", tool_result);
    
    // Clean up
    std::fs::remove_file("ops.db").ok();
    std::fs::remove_file("ops.db-shm").ok();
    std::fs::remove_file("ops.db-wal").ok();
    
    println!("Test completed successfully!");
    Ok(())
}