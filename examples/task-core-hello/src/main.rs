use task_core::{
    TaskSystem, TaskSystemBuilder, TaskConfig,
    model::{NewTaskSpec, TaskId, AgentId, Durability, TaskType, AgentError},
    executor::{Agent, TaskContext, AgentRegistry},
};
use dagger_macros::task_agent;
use task_core::AGENTS;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

/// Simple greeting agent
#[task_agent(
    name = "greeter",
    description = "Says hello to the input name"
)]
async fn greeter(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    // Parse input as string
    let name = String::from_utf8(input.to_vec())
        .map_err(|e| AgentError::User(format!("Invalid UTF-8: {}", e)))?;
    
    // Create greeting
    let greeting = format!("Hello, {}! Welcome to the high-performance task system.", name);
    info!("Greeter agent: {}", greeting);
    
    // Return as bytes
    Ok(Bytes::from(greeting))
}

/// Calculator agent that can add two numbers
#[task_agent(
    name = "calculator",
    description = "Adds two numbers from JSON input"
)]
async fn calculator(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    // Parse JSON input
    let data: serde_json::Value = serde_json::from_slice(&input)
        .map_err(|e| AgentError::User(format!("Invalid JSON: {}", e)))?;
    
    let a = data["a"].as_f64()
        .ok_or_else(|| AgentError::User("Missing field 'a'".to_string()))?;
    let b = data["b"].as_f64()
        .ok_or_else(|| AgentError::User("Missing field 'b'".to_string()))?;
    
    let result = a + b;
    info!("Calculator: {} + {} = {}", a, b, result);
    
    // Return result as JSON
    let output = serde_json::json!({ "result": result });
    Ok(Bytes::from(serde_json::to_vec(&output).unwrap()))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Setup logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    
    info!("Starting task-core hello world example");
    
    // Create configuration
    let config = TaskConfig::development();
    
    // Build task system
    let mut system_builder = TaskSystemBuilder::new()
        .with_storage_path("hello_tasks.db")
        .with_config(config);
    
    // Register agents from linkme
    let mut registry = AgentRegistry::new();
    for register_fn in task_core::AGENTS {
        register_fn(&mut registry);
    }
    
    // Build and start system
    let system = Arc::new(system_builder.build(Arc::new(registry)).await?);
    let system_clone = system.clone();
    
    // Start the system in background
    let handle = tokio::spawn(async move {
        system_clone.run().await
    });
    
    // Wait for system to start
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // Example 1: Simple greeting
    info!("=== Example 1: Simple Greeting ===");
    let greeting_task = NewTaskSpec {
        agent: greeterAgent::AGENT_ID,
        input: Bytes::from("World"),
        dependencies: vec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Greet the world"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(3),
        parent: None,
    };
    
    let task1_id = system.submit_task(greeting_task).await?;
    info!("Submitted greeting task: {}", task1_id);
    
    // Example 2: Calculator
    info!("\n=== Example 2: Calculator ===");
    let calc_input = serde_json::json!({
        "a": 10.5,
        "b": 20.3
    });
    
    let calc_task = NewTaskSpec {
        agent: calculatorAgent::AGENT_ID,
        input: Bytes::from(serde_json::to_vec(&calc_input)?),
        dependencies: vec![],
        durability: Durability::AtMostOnce,
        task_type: TaskType::Task,
        description: Arc::from("Calculate sum"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(1),
        parent: None,
    };
    
    let task2_id = system.submit_task(calc_task).await?;
    info!("Submitted calculator task: {}", task2_id);
    
    // Wait for tasks to complete
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // Check task status
    if let Some(task1) = system.get_task(task1_id).await? {
        info!("Task 1 status: {:?}", task1.status);
        if let Some(output) = system.storage.get_output(task1_id).await? {
            let result = String::from_utf8(output.to_vec())?;
            info!("Task 1 output: {}", result);
        }
    }
    
    if let Some(task2) = system.get_task(task2_id).await? {
        info!("Task 2 status: {:?}", task2.status);
        if let Some(output) = system.storage.get_output(task2_id).await? {
            let result: serde_json::Value = serde_json::from_slice(&output)?;
            info!("Task 2 output: {}", result);
        }
    }
    
    // Show statistics
    info!("\n=== System Statistics ===");
    let stats = system.scheduler_stats().await?;
    info!("Scheduler stats: {:?}", stats);
    
    // Shutdown
    info!("\nShutting down...");
    system.shutdown().await?;
    handle.abort();
    
    Ok(())
}