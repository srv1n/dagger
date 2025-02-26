
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;

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
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let question = message.payload["question"].as_str().ok_or(anyhow!("Missing question"))?;
    let answer = message.payload["intermediate_answer"].as_str().ok_or(anyhow!("Missing answer"))?;

    let question_eval_prompt = format!("Determine evaluation types for: {}", question);
    let question_eval_response = llm_generate(&question_eval_prompt, "evaluator_question").await?;
    let needs_completeness = question_eval_response["needsCompleteness"].as_bool().unwrap_or(false);

    let mut all_results = Vec::new();
    let definitive_prompt = format!("Is this answer definitive?\nQuestion: {}\nAnswer: {}", question, answer);
    let definitive_response = llm_generate(&definitive_prompt, "evaluator_definitive").await?;
    all_results.push(definitive_response.clone());

    if needs_completeness {
        let completeness_prompt = format!("Does this answer cover all aspects of the question?\nQuestion: {}\nAnswer: {}", question, answer);
        let completeness_response = llm_generate(&completeness_prompt, "evaluator_completeness").await?;
        all_results.push(completeness_response);
    }

    let mut evaluation_results_json = Vec::new();
    let mut all_passed = true;
    for result in &all_results {
        let pass = result["pass"].as_bool().unwrap_or(false);
        let think = result["think"].as_str().unwrap_or("").to_string();
        evaluation_results_json.push(json!({"pass": pass, "reason": think}));
        if !pass {
            all_passed = false;
        }
    }

    executor
        .publish(
            "evaluation_results",
            Message::new(node_id.to_string(), json!({"evaluation_results": evaluation_results_json, "question": question})),
            cache,
        )
        .await?;

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
            for query in &gap_queries {
                append_global_value(
                    cache,
                    "global",
                    "task_queue",
                    json!({"type": "search", "query": query, "source": "gap", "attempt_count": 0}),
                )?;
            }
            executor
                .publish(
                    "gap_questions",
                    Message::new(node_id.to_string(), json!({"gap_questions": gap_queries, "query": question})),
                    cache,
                )
                .await?;
        }
    }

    // Notify Supervisor of task completion
    executor
        .publish(
            "task_completed",
            Message::new(node_id.to_string(), json!({"question": question})),
            cache,
        )
        .await?;

    Ok(())

}