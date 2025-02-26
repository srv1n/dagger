use anyhow::Result;
use dagger::{Cache, Message, PubSubExecutor};

use serde_json::json;
use std::sync::Arc;
use tokio::time::{sleep, Duration, timeout};
use tracing::info;
use tracing_subscriber;
use deepresearch::agents::get_all_agents;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cache = Arc::new(Cache::new(std::collections::HashMap::new()));
    let mut executor = PubSubExecutor::new(None, "pubsub_db", cache.clone()).await?;

    executor.register_agents(get_all_agents()).await?;

    let mut outcome_rx = executor.start().await?;
    let initial_query = "Research the best electric vehicles (EVs) for families in 2024, considering price, range, safety, and cargo space.";
    let initial_message = Message::new("initial_input".to_string(), json!({"query": initial_query}));
    executor.publish("start", initial_message, &cache, None).await?;

    // Wait for workflow completion with a reasonable timeout
    let duration = Duration::from_secs(120); // Increased to allow more processing time
    let result = timeout(duration, async {
        while let Some(outcome) = outcome_rx.recv().await {
            info!("Outcome: {:?}", outcome);
            if outcome.node_id == "SupervisorAgent" && !outcome.success {
                info!("Workflow completed or failed, shutting down");
                break;
            }
        }
    }).await;

    if result.is_err() {
        info!("Timeout reached, shutting down");
    }

    executor.stop().await?;
    let dot = executor.serialize_tree_to_dot("workflow1", &cache).await.unwrap();
    println!("DOT Graph:\n{}", dot);
    println!("Final Cache: {:#?}", cache.read().unwrap());
    Ok(())
}