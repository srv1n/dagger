use anyhow::Result;
use dagger::taskagent::{TaskSystemBuilder};
use dagger::task_agent;
use serde_json::{json, Value};

/// Define a simple task agent
#[task_agent(
    name = "simple_agent", 
    description = "A simple agent that processes input text",
    input_schema = r#"{"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}"#,
    output_schema = r#"{"type": "object", "properties": {"result": {"type": "string"}}, "required": ["result"]}"#
)]
async fn simple_agent(input: Value, task_id: &str, job_id: &str) -> Result<Value, String> {
    // Extract input text
    let text = input["text"].as_str().unwrap_or("No text provided");
    println!("Processing text: {}", text);
    
    // Simple processing - convert to uppercase
    let processed = text.to_uppercase();
    
    // Return the result
    Ok(json!({"result": processed}))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set up logging
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set default tracing subscriber");
    
    // Create a task system with one line using the builder
    let mut task_system = TaskSystemBuilder::new()
        .register_agent("simple_agent")?
        .build()?;
    
    // Run a simple objective
    let result = task_system.run_objective(
        "Process some text",
        "simple_agent",
        json!({"text": "Hello, world!"}),
    ).await?;
    
    println!("Task completed with result: {}", result);
    
    Ok(())
} 