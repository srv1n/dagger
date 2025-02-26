use chrono::Utc;
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;
use tokio::time::{sleep, Duration};

use crate::utils::search::read_page_content;

#[pubsub_agent(
    name = "ReaderAgent",
    subscribe = "search_results",
    publish = "page_content, knowledge",
    input_schema = r#"{"type": "object", "properties": {"search_results": {"type": "array"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"page_content": {"type": "string"}, "knowledge": {"type": "array"}}}"#
)]
pub async fn reader_agent(
    node_id: &str,
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    
    let search_results = message.payload["search_results"]
        .as_array()
        .ok_or(anyhow!("Missing search results"))?;

    for result in search_results {
        let url = result["url"].as_str().ok_or(anyhow!("Missing URL"))?;
        let content = read_page_content(url).await?;

        sleep(Duration::from_secs(2)).await;

        executor
            .publish(
                "page_content",
                Message::new(node_id.to_string(), json!({"page_content": content, "url": url})),
                cache,
                Some(("task_queue".to_string(),json!({"page_content": content, "url": url})))
            )
            .await?;

        let knowledge_item = json!({
            "id": format!("knowledge_{}", url),
            "question": "Content from URL",
            "answer": content,
            "source_urls": [url],
            "source_type": "webpage",
            "confidence": 0.7,
            "timestamp": Utc::now().to_rfc3339(),
        });

        append_global_value(cache, "global", "knowledge_base", knowledge_item.clone())?;
        append_global_value(
            cache,
            "global",
            "task_queue",
            json!({"type": "reason", "question": "Content from URL", "relevant_urls": [url], "relevant_knowledge_ids": [knowledge_item["id"]], "status": "pending"}),
        )?;

        executor
            .publish(
                "knowledge",
                Message::new(node_id.to_string(), json!({"knowledge": [knowledge_item]})),
                cache,
                Some(("task_queue".to_string(),json!({"knowledge": [knowledge_item]})))
            )
            .await?;
    }

    Ok(())
}