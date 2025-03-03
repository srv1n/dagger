// src/agents/reasoning.rs (Corrected)
use anyhow::anyhow;
use anyhow::Result;
use dagger::{append_global_value, get_global_input, Cache, Message, PubSubExecutor};
use dagger_macros::pubsub_agent;
use serde_json::json;
use tracing::info;

use crate::utils::llm::llm_generate;

#[pubsub_agent(
    name = "ReasoningAgent",
    subscribe = "knowledge",
    publish = "intermediate_answers",
    input_schema = r#"{"type": "object", "properties": {"knowledge": {"type": "array"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"intermediate_answer": {"type": "string"}}}"#
)]
pub async fn reasoning_agent(
    node_id: &str,
    channel: &str, // Use channel
    message: &Message, // Correct: &Message
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let task_id = message.task_id.clone().ok_or(anyhow!("Message is missing task_id"))?;

    let mut task = executor
        .task_manager
        .get_task_by_id(&task_id)
        .await
        .ok_or(anyhow!("Task not found"))?;

    let question = task.data["question"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let relevant_knowledge_ids: Vec<String> = task.data["relevant_knowledge_ids"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let knowledge_base: Vec<serde_json::Value> =
        get_global_input(&cache, "global", "knowledge_base")?;
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
    let answer = reasoning_response["answer"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let eval_task_id = format!("eval_{}", chrono::Utc::now().timestamp_nanos());
    executor
        .task_manager
        .add_task(
            eval_task_id.clone(),
            "Evaluate answer".to_string(),
            "EvaluationAgent".to_string(),
            vec![task_id.clone()], // Depends on reasoning
            vec![],
            json!({
                "answer": answer,
                "question": question,
                "sources": relevant_knowledge_ids
            }),
        )
        .await?;

    task.subtasks.push(eval_task_id.clone());
    executor.task_manager.update_task(&task_id, task).await?;

    executor
        .publish(
            "intermediate_answers", // correct channel
            Message {
                timestamp: chrono::Local::now().naive_local(),
                source: node_id.to_string(),
                channel: Some("intermediate_answers".to_string()),
                task_id: Some(eval_task_id), // eval task
                message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                payload: json!({ "intermediate_answer": answer, "question": question }),
            },
        )
        .await?;

    Ok(())
}