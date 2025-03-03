// src/agents/search.rs (Corrected)
use anyhow::anyhow;
use anyhow::Result;
use dagger::TaskStatus;
use dagger::{Cache, Message, PubSubExecutor};
use dagger_macros::pubsub_agent;
use serde_json::json;
use serde_json::Value;
use tracing::info;

use crate::utils::search::perform_search;

#[pubsub_agent(
    name = "SearchAgent",
    subscribe = "search_queries",
    publish = "search_results",
    input_schema = r#"{"type": "object", "properties": {"search_queries": {"type": "array"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"search_results": {"type": "array"}}}"#
)]
pub async fn search_agent(
    node_id: &str,
    channel: &str, // Use the channel!
    message: &Message, // Correct: &Message
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let query: String = message.payload["query"]
        .as_str()
        .ok_or(anyhow!("Missing query"))?
        .to_string();

    let task_id = message
        .task_id
        .clone()
        .ok_or(anyhow!("Message is missing task_id"))?;

    // let mut all_results = Vec::new();

    let results: Vec<Value> = perform_search(&query).await?;
    // let all_results = results.iter().map(|v| v.to_string()).collect();

    // all_results.extend(results);

    executor
        .publish(
            "search_results", // Use correct channel
            Message {
                timestamp: chrono::Local::now().naive_local(),
                source: node_id.to_string(),
                channel: Some("search_results".to_string()),
                task_id: Some(task_id.clone()), // Keep task_id
                message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                payload: json!({ "search_results": results }),
            },
        )
        .await?;
    executor.task_manager.update_task_status(&task_id, TaskStatus::Completed).await?;
    info!("Search agent completed");
    Ok(())
}