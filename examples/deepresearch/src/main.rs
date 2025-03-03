// src/main.rs
use anyhow::Result;
use dagger::{Cache, Message, NodeExecutionOutcome, PubSubExecutor, PubSubWorkflowSpec};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::{sleep, Duration};
use tracing::info;
use tracing_subscriber;
use deepresearch::agents::get_all_agents;



#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cache = Arc::new(Cache::new());
    let mut executor = PubSubExecutor::new(None, "./sled_db", cache.clone()).await?;

    executor.register_agents(get_all_agents()).await?;
    println!("Registered Agents: {:?}", executor.list_agents().await);
    println!("Channels: {:?}", executor.list_channels().await);

    let initial_query = "Research the best electric vehicles (EVs) for families in 2024, considering price, range, safety, and cargo space.";
    let initial_message = Message::new("initial_input".to_string(), json!({ "query": initial_query }));

    let (cancel_tx, cancel_rx) = oneshot::channel();
    let spec = PubSubWorkflowSpec::EventDriven {
        channel: "start".to_string(),
        initial_message,
    };

    let executor_handle = {
        let mut executor = executor.clone();
        tokio::spawn(async move {
            let (report, outcome_rx) = executor.execute(spec, cancel_rx).await?;
            Ok::<_, anyhow::Error>((report, outcome_rx))
        })
    };

    let (report, mut outcome_rx) = executor_handle.await??;
    let mut last_outcome_time = std::time::Instant::now();
    let timeout = Duration::from_secs(10);

    loop {
        match tokio::time::timeout(Duration::from_secs(1), outcome_rx.recv()).await {
            Ok(Some(outcome)) => {
                println!("Outcome: {:?}", outcome);
                last_outcome_time = std::time::Instant::now();
                if !outcome.success {
                    println!("Task failed for {}: {:?}", outcome.node_id, outcome.final_error);
                }
            }
            Ok(None) => {
                println!("Outcome channel closed");
                break;
            }
            Err(_) => {
                let elapsed = last_outcome_time.elapsed();
                if elapsed > timeout {
                    println!("No outcomes for {} seconds, investigating...", elapsed.as_secs());
                    println!("Cache contents:");
                    for entry in cache.data.iter() {
                        println!("  Key: {:?}, Value: {:?}", entry.key(), entry.value());
                    }
                    let dot = executor.serialize_tree_to_dot("workflow_1", &cache).await?;
                    println!("DOT Diagram:\n{}", dot);
                    let _ = cancel_tx.send(());
                    break;
                } else {
                    info!("Waiting, elapsed: {}s", elapsed.as_secs());
                }
            }
        }
    }

    println!("Final Report: {:?}", report);
    Ok(())
}