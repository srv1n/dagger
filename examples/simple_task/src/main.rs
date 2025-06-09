use anyhow::Result;
use dagger::{
    Cache, DagExecutor, ExecutionMode, WorkflowSpec,
    insert_value, register_action,
    Node, NodeAction,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

// Simple action for demonstration
async fn hello_world(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let message = "Hello from Dagger!";
    insert_value(cache, &node.id, "output", message)?;
    println!("Executed: {}", message);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(None, registry.clone(), "simple_db")?;

    // Register action
    register_action!(executor, "hello_world", hello_world);

    let cache = Cache::new();
    let (_cancel_tx, cancel_rx) = oneshot::channel::<()>();

    println!("=== Demonstrating New Simplified API ===\n");

    // Method 1: New simplified methods (RECOMMENDED)
    println!("1. Using simplified methods:");
    println!("   executor.execute_static_dag(\"my_workflow\", &cache, cancel_rx)");
    println!("   executor.execute_agent_dag(\"analyze_task\", &cache, cancel_rx)\n");

    // Method 2: Using ExecutionMode enum
    println!("2. Using ExecutionMode:");
    println!("   executor.execute_dag_with_mode(ExecutionMode::Static, \"my_workflow\", &cache, cancel_rx)");
    println!("   executor.execute_dag_with_mode(ExecutionMode::Agent, \"analyze_task\", &cache, cancel_rx)\n");

    // Method 3: Old enum style (DEPRECATED but still supported)
    println!("3. Old enum style (deprecated):");
    println!("   executor.execute_dag(WorkflowSpec::Static {{ name: \"my_workflow\".to_string() }}, &cache, cancel_rx)");
    println!("   executor.execute_dag(WorkflowSpec::Agent {{ task: \"analyze_task\".to_string() }}, &cache, cancel_rx)\n");

    println!("=== Benefits of Simplification ===");
    println!("✅ Less verbose: Just pass strings instead of enum variants");
    println!("✅ Cleaner API: Purpose-specific methods");
    println!("✅ Backward compatible: Old WorkflowSpec still works");
    println!("✅ Type safety: ExecutionMode prevents confusion");

    Ok(())
} 