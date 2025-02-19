use anyhow::{Error, Result};
use dagger::{
    insert_value, parse_input_from_name, register_action, serialize_cache_to_prettyjson, Cache,
    DagConfig, DagExecutionReport, DagExecutor, HumanInterrupt, InfoRetrievalAgent, Node,
     WorkflowSpec,
};
use std::collections::HashMap;
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
mod supervisor;
use supervisor::SupervisorStep;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup tracing subscriber
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // Configure DagConfig for the agentic flow
    let config = DagConfig {
        max_iterations: Some(5),         // Limit to 5 iterations
        timeout_seconds: Some(30),       // 30-second total timeout
        human_wait_minutes: Some(1),     // Wait 1 minute for human input
        human_timeout_action: dagger::HumanTimeoutAction::Autopilot, // Proceed if no input
        review_frequency: Some(2),       // Human review every 2 iterations
        ..Default::default()
    };

    // Initialize the DAG executor with custom config
    let mut executor = DagExecutor::new(Some(config))?;

    // Register built-in actions
    register_action!(executor, "supervisor_step", SupervisorStep::new);
    register_action!(executor, "human_interrupt", HumanInterrupt::new);
    register_action!(executor, "info_retrieval", InfoRetrievalAgent::new);

    // Initialize the cache
    let cache = Cache::new(HashMap::new());

    // Create cancellation channel
    let (cancel_tx, cancel_rx) = oneshot::channel();

    // Start the agentic flow
    let task = "Plan a weekend trip to Paris";
    println!("Starting agentic flow for task: {}", task);

    // Spawn a task to simulate human input after 5 seconds
    let cache_clone = cache.clone();
    let executor_clone = executor.clone();
    tokio::spawn(async move {
        sleep(Duration::from_secs(5)).await;
        let instruction = serde_json::json!({
            "action": "retrieve_info",
            "params": {"query": "Visit Louvre"},
            "priority": 1
        });
        executor_clone.update_cache(
            task,
            "instruction_1".to_string(),
            dagger::SerializableData {
                value: instruction.to_string(),
            },
        )?;
        println!("Human input: Added instruction to visit Louvre");
        Ok::<(), Error>(())
    });

    // Execute the agentic DAG
    let report = executor
        .execute_dag(WorkflowSpec::Agent { task: task.to_string() }, &cache, cancel_rx)
        .await?;

    // Print results
    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Final Cache:\n{}", json_output);
    println!("Execution Report: {:#?}", report);

    // Optional: Visualize execution tree
    let dot_output = executor.serialize_tree_to_dot(task)?;
    println!("Execution Tree (DOT):\n{}", dot_output);

    Ok(())
}