
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;
use tokio::time::{sleep, Duration};

use crate::utils::memory::get_pending_tasks;

#[pubsub_agent(
    name = "SupervisorAgent",
    subscribe = "start, task_completed, final_answer",
    publish = "initial_query, report_request",
    input_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}, "report_request": {"type": "boolean"}}}"#
)]
pub async fn supervisor_agent(
    node_id: &str,
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    match channel {
        "start" => {
            let query = message.payload["query"].as_str().ok_or(anyhow!("Missing query"))?;
            info!("Supervisor received initial query: {}", query);

            // Initialize global state
            insert_global_value(cache, "global", "task_queue", Vec::<serde_json::Value>::new())?;
            insert_global_value(cache, "global", "knowledge_base", Vec::<serde_json::Value>::new())?;
            insert_global_value(cache, "global", "planned_tasks", 1)?;
            insert_global_value(cache, "global", "completed_tasks", 0)?;

            append_global_value(
                cache,
                "global",
                "task_queue",
                json!({"type": "search", "query": query, "source": "initial", "attempt_count": 0, "status": "pending"}),
            )?;

            executor
                .publish(
                    "initial_query",
                    Message::new(node_id.to_string(), json!({"query": query})),
                    cache,
                    Some(
                    ("task_queue".to_string(),json!({"query": query}))
                ))
                .await?;
        }
        "task_completed" => {
            let pending_tasks: Vec<serde_json::Value> = get_pending_tasks(cache)?;
            let knowledge_base: Vec<serde_json::Value> = get_global_input(cache, "global", "knowledge_base")?;
            let planned_tasks: usize = get_global_input(cache, "global", "planned_tasks")?;
            let completed_tasks: usize = get_global_input(cache, "global", "completed_tasks").unwrap_or(0);

            info!(
                "Task completed. Pending tasks: {}, Knowledge size: {}, Planned: {}, Completed: {}",
                pending_tasks.len(), knowledge_base.len(), planned_tasks, completed_tasks
            );

            if pending_tasks.is_empty() && completed_tasks >= planned_tasks {
                info!("All tasks completed, requesting report");
                executor
                    .publish(
                        "report_request",
                        Message::new(node_id.to_string(), json!({"report_request": true})),
                        cache,None
                    )
                    .await?;
            }
        }
        "final_answer" => {
            info!("Supervisor received final answer: {}", message.payload);
            executor.stop().await?;
        }
        _ => {}
    }
    Ok(())
}