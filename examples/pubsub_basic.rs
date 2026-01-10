//! Minimal Pub/Sub example using macro-based agents

use dagger::{pubsub_agent, Message, PubSubExecutor, PubSubWorkflowSpec};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

#[pubsub_agent(name = "printer", subscribe = "events", publish = "results")]
async fn printer(msg: Message) -> anyhow::Result<()> {
    println!("printer received: {}", msg.payload);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut executor = PubSubExecutor::new(None);
    executor
        .register_agent(Arc::new(PrinterPubSubAgent))
        .await?;

    let cache = dagger::Cache::new();
    let (cancel_tx, cancel_rx) = oneshot::channel();

    let message = Message::new("main".to_string(), serde_json::json!({"hello": "world"}));
    let workflow = PubSubWorkflowSpec::StartWith {
        channel: "events".to_string(),
        message,
    };

    let handle = tokio::spawn(async move { executor.execute(workflow, &cache, cancel_rx).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = cancel_tx.send(());
    let _ = handle.await;

    Ok(())
}
