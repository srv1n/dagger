
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;



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
    match channel {
        "start" => {
            let query = message.payload["query"].as_str().ok_or(anyhow!("Missing query"))?;
            info!("Supervisor received initial query: {}", query);

            // Initialize global state
            insert_global_value(cache, "global", "task_queue", Vec::<serde_json::Value>::new())?;
            insert_global_value(cache, "global", "knowledge_base", Vec::<serde_json::Value>::new())?;
            insert_global_value(cache, "global", "planned_tasks", 1)?; // Track initial query

            append_global_value(
                cache,
                "global",
                "task_queue",
                json!({"type": "search", "query": query, "source": "initial", "attempt_count": 0}),
            )?;

            executor
                .publish(
                    "initial_query",
                    Message::new(node_id.to_string(), json!({"query": query})),
                    cache,
                )
                .await?;
        }
        "task_completed" => {
            let task_queue: Vec<serde_json::Value> = get_global_input(cache, "global", "task_queue")?;
            let knowledge_base: Vec<serde_json::Value> = get_global_input(cache, "global", "knowledge_base")?;
            let planned_tasks: usize = get_global_input(cache, "global", "planned_tasks")?;

            info!("Task completed. Queue size: {}, Knowledge size: {}", task_queue.len(), knowledge_base.len());

            if task_queue.is_empty() && knowledge_base.len() >= planned_tasks {
                info!("All tasks completed, requesting report");
                executor
                    .publish(
                        "report_request",
                        Message::new(node_id.to_string(), json!({"report_request": true})),
                        cache,
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