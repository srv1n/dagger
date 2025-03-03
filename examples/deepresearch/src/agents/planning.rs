// src/agents/planning.rs (Corrected)
use anyhow::anyhow;
use anyhow::Result;
use dagger::TaskStatus;
use dagger::{Cache, Message, PubSubExecutor};
use dagger_macros::pubsub_agent;
use serde_json::json;
use tracing::info;

use crate::utils::llm::llm_generate;

#[pubsub_agent(
    name = "PlanningAgent",
    subscribe = "initial_query, gap_questions",
    publish = "search_queries",
    input_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"search_queries": {"type": "array"}}}"#
)]
pub async fn planning_agent(
    node_id: &str,
    channel: &str,     // Use the channel
    message: &Message, // Corrected: &Message
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    match channel {
        "initial_query" | "gap_questions" => {
            let query = message.payload["query"]
                .as_str()
                .ok_or(anyhow!("Missing query"))?
                .to_string();

            let task_id = message.task_id.clone().ok_or(anyhow!("Missing task_id"))?;

            let rewriter_prompt =
                format!("Rewrite this query for better search results:\n\n{}", query);
            let rewriter_response = llm_generate(&rewriter_prompt, "query_rewriter").await?;
            let expanded_queries: Vec<String> = rewriter_response["queries"]
                .as_array()
                .ok_or(anyhow!("Invalid query rewriter response"))?
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();

            let mut subtask_ids = Vec::new();
            for expanded_query in &expanded_queries {
                let subtask_id = format!("search_{}", chrono::Utc::now().timestamp_nanos());
                executor
                    .task_manager
                    .add_task(
                        subtask_id.clone(),
                        "Perform web search".to_string(),
                        "SearchAgent".to_string(),
                        vec![task_id.clone()],
                        vec![],
                        json!({ "query": expanded_query }),
                    )
                    .await?;
                subtask_ids.push(subtask_id.clone());

                executor
                    .publish(
                        "search_queries", // Correct channel
                        Message {
                            timestamp: chrono::Local::now().naive_local(),
                            source: node_id.to_string(),
                            channel: Some("search_queries".to_string()),
                            task_id: Some(task_id.clone()), // Use original task_id
                            message_id: subtask_id.clone(),
                            payload: json!({ "query": expanded_query }),
                        },
                    )
                    .await?;
            }

            if let Some(mut task) = executor.task_manager.get_task_by_id(&task_id).await {
                task.subtasks.extend(subtask_ids.clone());
                executor.task_manager.update_task(&task_id, task).await?;
            }

            executor
                .task_manager
                .update_task_status(&task_id, TaskStatus::Completed)
                .await?;
        }
        _ => {}
    }
    Ok(())
}
