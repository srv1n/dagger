// src/agents/supervisor.rs (Corrected)
use anyhow::anyhow;
use anyhow::Result;
use dagger::{Cache, Message, PubSubExecutor, TaskStatus};
use dagger_macros::pubsub_agent;
use serde_json::json;
use tracing::info;

#[pubsub_agent(
    name = "SupervisorAgent",
    subscribe = "start, task_completed, final_answer",
    publish = "initial_query, report_request",
    input_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {}}"#
)]
pub async fn supervisor_agent(
    node_id: &str,
    channel: &str, // Use the channel
    message: &Message, // Corrected: &Message
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    match channel {
        "start" => {
            let query = message.payload["query"]
                .as_str()
                .ok_or(anyhow!("Missing query"))?
                .to_string();
            info!("Supervisor received initial query: {}", query);

            let task_id = "initial_research".to_string();
            executor
                .task_manager
                .add_task(
                    task_id.clone(),
                    "Initial research task".to_string(),
                    "PlanningAgent".to_string(),
                    vec![],
                    vec![],
                    json!({ "query": query }),
                )
                .await?;

            executor
                .publish(
                    "initial_query",  // Use correct channel
                    Message {
                        timestamp: chrono::Local::now().naive_local(),
                        source: node_id.to_string(),
                        channel: Some("initial_query".to_string()),
                        task_id: Some(task_id), // Associate message with task
                        message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                        payload: json!({ "query": query }),
                    },
                )
                .await?;
        }
        "task_completed" => {
            let completed_tasks = executor
                .task_manager
                .get_tasks_by_status(TaskStatus::Completed)
                .await;
            let failed_tasks = executor
                .task_manager
                .get_tasks_by_status(TaskStatus::Failed)
                .await;
           let all_tasks = executor.task_manager.get_task_count().await;


            info!(
                "Task completed. Completed: {}, Failed: {}, Total {}",
                completed_tasks.len(),
                failed_tasks.len(),
                all_tasks
            );

            if completed_tasks.len() + failed_tasks.len() >= all_tasks {
                if failed_tasks.is_empty() {
                    info!("All tasks completed, requesting report");
                    executor
                        .publish(
                            "report_request", // Use correct channel
                            Message {
                                timestamp: chrono::Local::now().naive_local(),
                                source: node_id.to_string(),
                                channel: Some("report_request".to_string()),
                                task_id: None,
                                message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                                payload: json!({ "report_request": true }),
                            },
                        )
                        .await?;
                } else {
                    info!("Some tasks failed. Stopping.");
                    executor.stop().await?;
                }
            } else {
               let dot = executor.serialize_tree_to_dot("workflow_1", &cache).await?;
                println!("DOT Diagram:\n{}", dot);
                let tasks = executor.task_manager.get_all_tasks().await;
                info!("Tasks: {:#?}", tasks);

                let cache_data = cache.data.clone();
                info!("Cache Data: {:#?}", cache_data);
            }
        }
        "final_answer" => {
            info!("Supervisor received final answer: {}", message.payload);
            executor.stop().await?; // Stop the executor.
        }
        _ => {}
    }
    Ok(())
}