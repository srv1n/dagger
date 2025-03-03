// src/agents/evaluation.rs (Corrected)
use anyhow::anyhow;
use anyhow::Result;
use dagger::TaskStatus; // Import TaskStatus
use dagger::{Cache, Message, PubSubExecutor};
use dagger_macros::pubsub_agent;
use serde_json::json;
use tracing::info;

use crate::utils::llm::llm_generate;

#[pubsub_agent(
    name = "EvaluationAgent",
    subscribe = "intermediate_answers",
    publish = "evaluation_results, gap_questions, task_completed",
    input_schema = r#"{"type": "object", "properties": {"intermediate_answer": {"type": "string"}, "question": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"evaluation_results": {"type": "array"}, "gap_questions": {"type": "array"}}}"#
)]
pub async fn evaluation_agent(
    node_id: &str,
    channel: &str, // Use the channel argument
    message: &Message,  // Corrected: Use &Message
    executor: &mut PubSubExecutor,
    cache: &Cache, // Use Arc<Cache>
) -> Result<()> {
    info!("EvaluationAgent started");
    let question = message.payload["question"]
        .as_str()
        .ok_or(anyhow!("Missing question"))?
        .to_string();
    let answer = message.payload["intermediate_answer"]
        .as_str()
        .ok_or(anyhow!("Missing answer"))?
        .to_string();

    let task_id = message
        .task_id
        .clone()
        .ok_or(anyhow!("Message is missing task_id"))?;


    let question_eval_prompt = format!("Determine evaluation types for: {}", question);
    let question_eval_response = llm_generate(&question_eval_prompt, "evaluator_question").await?;
    let needs_completeness = question_eval_response["needsCompleteness"]
        .as_bool()
        .unwrap_or(false);

    let mut all_results = Vec::new();
    let definitive_prompt =
        format!("Is this answer definitive?\nQuestion: {}\nAnswer: {}", question, answer);
    let definitive_response = llm_generate(&definitive_prompt, "evaluator_definitive").await?;
    all_results.push(definitive_response.clone());

    if needs_completeness {
        let completeness_prompt = format!(
            "Does this answer cover all aspects of the question?\nQuestion: {}\nAnswer: {}",
            question, answer
        );
        let completeness_response =
            llm_generate(&completeness_prompt, "evaluator_completeness").await?;
        all_results.push(completeness_response);
    }

    let mut evaluation_results_json = Vec::new();
    let mut all_passed = true;
    for result in &all_results {
        let pass = result["pass"].as_bool().unwrap_or(false);
        let think = result["think"].as_str().unwrap_or("").to_string();
        evaluation_results_json.push(json!({ "pass": pass, "reason": think }));
        if !pass {
            all_passed = false;
        }
    }
    let evaluation_results = json!(evaluation_results_json);
    info!("EvaluationAgent Step 1 completed");

    executor
        .publish(
            "evaluation_results",  // Use the correct channel names.
            Message {
                timestamp: chrono::Local::now().naive_local(),
                source: node_id.to_string(),
                channel: Some("evaluation_results".to_string()),
                task_id: Some(task_id.clone()), // Keep the task_id
                message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                payload: json!({
                    "evaluation_results": evaluation_results,
                    "question": question
                }),
            },
        )
        .await?;
    info!("EvaluationAgent Step 2 completed");
    if needs_completeness && !all_passed {
        let gap_prompt = format!("Identify knowledge gaps for this question: {}", question);
        let gap_response = llm_generate(&gap_prompt, "planning_agent_gap").await?;
        let gap_queries: Vec<String> = gap_response["queries"]
            .as_array()
            .ok_or(anyhow!("Invalid gap response"))?
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        if !gap_queries.is_empty() {
            for gap_query in &gap_queries {
                 let subtask_id = format!("gap_{}", chrono::Utc::now().timestamp_nanos());
                executor
                    .task_manager
                    .add_task(
                        subtask_id.clone(),
                        "Identify knowledge gaps".to_string(),
                        "PlanningAgent".to_string(),
                        vec![task_id.clone()],
                        vec![],
                        json!({ "query": gap_query }),
                    )
                    .await?;

                executor
                    .publish(
                        "gap_questions", // Use correct channel
                        Message {
                            timestamp: chrono::Local::now().naive_local(),
                            source: node_id.to_string(),
                            channel: Some("gap_questions".to_string()),
                            task_id: Some(subtask_id), // Correct task_id
                            message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                            payload: json!({ "query": gap_query }),
                        },
                    )
                    .await?;
            }

             if let Some(mut current_task) = executor.task_manager.get_task_by_id(&task_id).await {
                current_task.subtasks = gap_queries.iter().map(|q| format!("gap_{}", q)).collect();
                executor.task_manager.update_task(&task_id, current_task).await?;
            }
        }
    }
    info!("EvaluationAgent Step 3 completed");
    executor
        .publish(
            "task_completed",  // Use correct channel name
            Message {
                timestamp: chrono::Local::now().naive_local(),
                source: node_id.to_string(),
                channel: Some("task_completed".to_string()),
                task_id: Some(task_id.clone()), // Keep original task_id
                message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                payload: json!({ "question": question }),
            },
        )
        .await?;
    info!("EvaluationAgent Step 4 completed");
    // executor.task_manager.update_task_status(&task_id, TaskStatus::Completed).await?;
    info!("EvaluationAgent completed");
    Ok(())
}