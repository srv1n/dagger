// src/agents/reader.rs (Corrected)
use anyhow::anyhow;
use anyhow::Result;
use chrono::Utc;
use dagger::{append_global_value, Cache, Message, PubSubExecutor};
use dagger_macros::pubsub_agent;
use serde_json::json;
use tracing::info;

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
    channel: &str, // Use channel
    message: &Message, // Corrected: &Message
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let search_results = message.payload["search_results"]
        .as_array()
        .ok_or(anyhow!("Missing search results"))?;

    let parent_task_id = message.task_id.clone().ok_or(anyhow!("Missing task_id"))?;

    for result in search_results {
        let url = result["url"].as_str().ok_or(anyhow!("Missing URL"))?.to_string();

        let subtask_id = format!("read_{}", chrono::Utc::now().timestamp_nanos());
        executor
            .task_manager
            .add_task(
                subtask_id.clone(),
                "Read web page content".to_string(),
                "ReaderAgent".to_string(),
                vec![parent_task_id.clone()], // Correct dependency
                vec![],
                json!({ "url": url }),
            )
            .await?;

        if let Some(mut parent_task) = executor.task_manager.get_task_by_id(&parent_task_id).await
        {
            parent_task.subtasks.push(subtask_id.clone());
            executor
                .task_manager
                .update_task(&parent_task_id, parent_task)
                .await?;
        }

        let content = read_page_content(&url).await?;

        executor
            .publish(
                "page_content", // Correct channel
                Message {
                    timestamp: chrono::Local::now().naive_local(),
                    source: node_id.to_string(),
                    channel: Some("page_content".to_string()),
                    task_id: Some(subtask_id.clone()), // subtask_id
                    message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                    payload: json!({ "page_content": content, "url": url }),
                },
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

        append_global_value(&cache, "global", "knowledge_base", knowledge_item.clone())?;

        let reason_task_id = format!("reason_{}", chrono::Utc::now().timestamp_nanos());
        executor
            .task_manager
            .add_task(
                reason_task_id.clone(),
                "Reasoning task".to_string(),
                "ReasoningAgent".to_string(),
                vec![subtask_id.clone()], // Depends on read task
                vec![],
                json!({
                    "question": "Content from URL",
                    "relevant_urls": [url],
                    "relevant_knowledge_ids": [knowledge_item["id"]]
                }),
            )
            .await?;

        if let Some(mut read_task) = executor.task_manager.get_task_by_id(&subtask_id).await {
            read_task.subtasks.push(reason_task_id.clone());
            executor.task_manager.update_task(&subtask_id, read_task).await?;
        }

        executor
            .publish(
                "knowledge",  // Correct channel
                Message {
                    timestamp: chrono::Local::now().naive_local(),
                    source: node_id.to_string(),
                    channel: Some("knowledge".to_string()),
                    task_id: Some(reason_task_id), // reason task id
                    message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                    payload: json!({ "knowledge": [knowledge_item] }),
                },
            )
            .await?;
    }

    Ok(())
}