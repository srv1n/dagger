
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;
use tokio::time::{sleep, Duration};
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
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    sleep(Duration::from_secs(2)).await;
    let search_queries: Vec<String> = message.payload["search_queries"]
        .as_array()
        .ok_or(anyhow!("Missing search_queries"))?
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    let mut all_results = Vec::new();
    for query in search_queries {
        let search_results = perform_search(&query).await?;
        all_results.extend(search_results.clone());
        for result in search_results {
            let url = result["url"].as_str().unwrap().to_string();
            append_global_value(
                cache,
                "global",
                "task_queue",
                json!({"type": "read", "url": url, "source_query": query, "status": "pending"}),
            )?;
        }
    }

    executor
        .publish(
            "search_results",
            Message::new(node_id.to_string(), json!({"search_results": all_results})),
            cache,
            Some("search_results")
        )
        .await?;

    Ok(())
}