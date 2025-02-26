
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;
use tokio::time::{sleep, Duration};
use crate::utils::{llm::llm_generate, memory::update_global_task_queue};
#[pubsub_agent(
    name = "ReasoningAgent",
    subscribe = "knowledge, gap_questions",
    publish = "intermediate_answers, gap_questions",
    input_schema = r#"{"type": "object", "properties": {"knowledge": {"type": "array"}, "question": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"intermediate_answer": {"type": "string"}, "confidence": {"type": "number"}, "gap_questions": {"type": "array"}}}"#
)]
pub async fn reasoning_agent(
    node_id: &str,
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    sleep(Duration::from_secs(2)).await;
    let mut task_queue: Vec<serde_json::Value> = get_global_input(cache, "global", "task_queue")?;
    if let Some(index) = task_queue.iter().position(|task| task["type"].as_str() == Some("reason") && task["status"].as_str() == Some("pending")) {
        let mut reason_task = task_queue.remove(index);
        reason_task["status"] = json!("completed");
        task_queue.push(reason_task.clone()); // Add back with updated status
        update_global_task_queue(cache, task_queue)?;
        info!("Marked reason task as completed");

        let question = reason_task["question"].as_str().unwrap_or_default().to_string();
        let relevant_knowledge_ids: Vec<String> = reason_task["relevant_knowledge_ids"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let knowledge_base: Vec<serde_json::Value> = get_global_input(cache, "global", "knowledge_base")?;
        let mut relevant_knowledge = String::new();
        for item in knowledge_base {
            if let Some(id) = item["id"].as_str() {
                if relevant_knowledge_ids.contains(&id.to_string()) {
                    relevant_knowledge.push_str(item["answer"].as_str().unwrap_or_default());
                    relevant_knowledge.push('\n');
                }
            }
        }

        let reasoning_prompt = format!(
            "Based on the following knowledge, answer this question: {}\n\nKnowledge:\n{}",
            question, relevant_knowledge
        );
        let reasoning_response = llm_generate(&reasoning_prompt, "reasoning_agent").await?;
        let answer = reasoning_response["answer"].as_str().unwrap_or_default().to_string();

        append_global_value(
            cache,
            "global",
            "task_queue",
            json!({"type": "evaluate", "answer": answer, "question": question, "sources": relevant_knowledge_ids, "status": "pending"}),
        )?;

        // Increment completed tasks
        let completed_tasks: usize = get_global_input(cache, "global", "completed_tasks").unwrap_or(0);
        insert_global_value(cache, "global", "completed_tasks", completed_tasks + 1)?;
        info!("Incremented completed_tasks to: {}", completed_tasks + 1);

        executor
            .publish(
                "intermediate_answers",
                Message::new(node_id.to_string(), json!({"intermediate_answer": answer, "question": question})),
                cache,
                Some(("task_queue".to_string(),json!({"intermediate_answer": answer, "question": question}))
            )
            )
            .await?;
    }

    Ok(())
}